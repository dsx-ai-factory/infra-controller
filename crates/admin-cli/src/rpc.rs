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
use std::time::Duration;

use ::rpc::forge::instance_interface_config::NetworkDetails;
use ::rpc::forge::{
    self as rpc, BmcEndpointRequest, FindInstanceTypesByIdsRequest,
    FindNetworkSecurityGroupsByIdsRequest, FindPendingDpuServiceSyncsByIdsRequest,
    GetDpfHostSnapshotRequest, GetDpfStateRequest, GetNetworkSecurityGroupAttachmentsRequest,
    GetNetworkSecurityGroupPropagationStatusRequest, IdentifySerialRequest,
    ListDpuServiceSyncHistoryRequest, MachineHardwareInfo, MachineHardwareInfoUpdateType,
    ModifyDpfStateRequest, NetworkPrefix, NetworkSecurityGroupAttributes,
    NetworkSegmentCreationRequest, NetworkSegmentType, PendingDpuServiceSync,
    ReleaseDpuServiceSyncHoldRequest, Remediation, RemediationIdList, RemediationList,
    SpxPartitionSearchFilter, UpdateMachineHardwareInfoRequest, UpdateNetworkSecurityGroupRequest,
    VpcCreationRequest, VpcSearchFilter, VpcVirtualizationType, VpcsByIdsRequest,
};
use ::rpc::forge_api_client::ForgeApiClient;
use ::rpc::{Machine, NetworkSegment};
use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSubtypeTrait, MachineInterfaceId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::nvlink::{NvLinkLogicalPartitionId, NvLinkPartitionId};
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::spx::SpxPartitionId;
use carbide_uuid::switch::SwitchId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use futures::{StreamExt, TryStreamExt, stream};
use mac_address::MacAddress;

use crate::IntoOnlyOne;
use crate::admission_retry::retry_on_admission_exhaustion;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::expected_machines::common::{ExpectedMachineJson, HostDpuPolicy};
use crate::instance::AllocateInstance;
use crate::machine::MachineAutoupdate;

/// [`ApiClient`] is a thin wrapper around [`ForgeApiClient`], which mainly adds some convenience
/// methods.
#[derive(Clone)]
pub(crate) struct ApiClient(pub(crate) ForgeApiClient);

/// Returns `True` when `status` *can* mean the server does not implement
/// the requested RPC, telling the caller to retry through the deprecated alias.
///
/// API servers that predate a renamed RPC answer it in one of two ways:
///
/// - `Unimplemented`, when the request reaches the gRPC router.
/// - A bare HTTP 403 with no `grpc-status` trailer, when `carbide-api` RBAC
///   rules reject a method name missing from its permission table (which
///   happens before the gRPC router is even consulted). tonic maps that 403 to
///   `PermissionDenied` on the client.
fn maybe_unimplemented(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unimplemented | tonic::Code::PermissionDenied
    )
}

/// Caps `page_size` to a non-zero server `cap` (the `*ByIds` per-request limit).
/// A zero cap means the server enforces no limit, so `page_size` is used as-is --
/// chunking by zero would panic.
fn cap_chunk_size(page_size: usize, cap: usize) -> usize {
    if cap == 0 {
        page_size
    } else {
        page_size.min(cap)
    }
}

/// Legacy BMC fields sent with a full `ExpectedMachine` update.
///
/// `patch_expected_machine` still fetches the current record so it can merge
/// ordinary patch fields. These two fields need their own rules because the API
/// uses their presence to distinguish a legacy `--bmc-*` override from the
/// canonical `HostBmc` entry in `interfaces`.
#[derive(Clone, Debug, PartialEq)]
struct LegacyBmcPatchFields {
    bmc_ip_address: Option<String>,
    bmc_ip_allocation: Option<i32>,
}

/// `replacement_has_effective_host_bmc` resolves an omitted role the same way
/// the update API does before deciding whether top-level BMC fields are legacy
/// overrides. Otherwise a matching stored `HostBmc` is hidden at this point,
/// and replaying the compatibility fields would overwrite its nested changes.
fn replacement_has_effective_host_bmc(
    existing: &rpc::ExpectedMachine,
    replacement_interfaces: &[rpc::ExpectedInterface],
) -> bool {
    let existing_bmc_mac = existing.bmc_mac_address.parse::<MacAddress>().ok();

    replacement_interfaces
        .iter()
        .enumerate()
        .any(|(index, replacement)| {
            if let Some(role) = replacement.role {
                return rpc::ExpectedInterfaceRole::try_from(role).ok()
                    == Some(rpc::ExpectedInterfaceRole::HostBmc);
            }

            let Ok(mac_address) = replacement.mac_address.parse::<MacAddress>() else {
                return false;
            };
            let existing_interface = existing
                .interfaces()
                .get(index)
                .filter(|candidate| {
                    candidate.mac_address.parse::<MacAddress>().ok() == Some(mac_address)
                })
                .or_else(|| {
                    existing.interfaces().iter().find(|candidate| {
                        candidate.mac_address.parse::<MacAddress>().ok() == Some(mac_address)
                    })
                });

            match existing_interface {
                Some(existing_interface) => {
                    existing_interface
                        .role
                        .and_then(|role| rpc::ExpectedInterfaceRole::try_from(role).ok())
                        == Some(rpc::ExpectedInterfaceRole::HostBmc)
                }
                None => existing_bmc_mac == Some(mac_address),
            }
        })
}

/// `legacy_bmc_patch_fields` keeps old patch behavior unless the caller
/// supplies a `HostBmc` replacement or an explicit legacy override.
///
/// `Dynamic` and `Retained` use an empty address as an explicit clear. A
/// missing protobuf string cannot express that on a full update because it also
/// means the legacy flag was omitted.
fn legacy_bmc_patch_fields(
    existing: &rpc::ExpectedMachine,
    bmc_ip_address_override: Option<String>,
    bmc_ip_allocation_override: Option<rpc::BmcIpAllocationType>,
    replacement_interfaces: Option<&[rpc::ExpectedInterface]>,
) -> LegacyBmcPatchFields {
    let replaces_host_bmc = replacement_interfaces
        .is_some_and(|interfaces| replacement_has_effective_host_bmc(existing, interfaces));

    let clears_fixed_ip = bmc_ip_allocation_override.is_some_and(|allocation| {
        matches!(
            allocation,
            rpc::BmcIpAllocationType::Dynamic | rpc::BmcIpAllocationType::Retained
        )
    });

    let bmc_ip_address = match bmc_ip_address_override {
        Some(bmc_ip_address) => Some(bmc_ip_address),
        None if clears_fixed_ip => Some(String::new()),
        None if replaces_host_bmc => None,
        None => existing.bmc_ip_address.clone(),
    };
    let bmc_ip_allocation = match bmc_ip_allocation_override {
        Some(allocation) => Some(allocation as i32),
        None if replaces_host_bmc => None,
        None => existing.bmc_ip_allocation,
    };

    LegacyBmcPatchFields {
        bmc_ip_address,
        bmc_ip_allocation,
    }
}

// Benchmarks showed 4 had better overall performance while still overlapping page fetch latency.
const PAGED_LIST_FETCH_CONCURRENCY: usize = 4;

/// Attempt cap for retrying a single `RESOURCE_EXHAUSTED`-rejected call inside
/// [`ApiClient::get_all_instances`]'s paged fetch (the id listing and each id
/// chunk fetch individually), mirroring the per-instance/preflight caps used
/// elsewhere in this crate.
const MAX_PAGED_FETCH_ATTEMPTS: usize = 8;
/// Cumulative backoff cap for one such retried call.
const MAX_PAGED_FETCH_BACKOFF: Duration = Duration::from_secs(120);

// Note: You do *not* need to add every gRPC method to this wrapper. Callers can use `.0` to get
// access to the underlying ForgeApiClient, if they want to simply call the gRPC methods themselves.
// Add methods here if there's some value to it, like constructing rpc request objects from simpler
// primitives, or other data conversions.
//
// (this module used to have more logic around establishing a connection to carbide, but this is all
// now done in ForgeApiClient itself, leaving these methods only concerned with data conversions and
// other conveniences. 90% of these methods no longer justify their existence... we probably don't
// need to add more.)
impl ApiClient {
    /// Caps a CLI `page_size` to the server's `max_find_by_ids`, so a page of ids
    /// never exceeds what the `*ByIds` RPCs accept (they reject larger requests
    /// with `InvalidArgument`). The cap is read from `RuntimeConfig`, the same
    /// source `version` already exposes. A zero/unset cap means the server
    /// enforces no limit, so we fall back to `page_size` -- `chunks(0)` panics.
    pub(crate) async fn effective_chunk_size(&self, page_size: usize) -> CarbideCliResult<usize> {
        // Every `*_by_ids` paged fetch calls this first to size its chunks, so a
        // `RESOURCE_EXHAUSTED` rejection here needs the same retry protection as the
        // paged fetches themselves -- otherwise it's a single unretried call sitting
        // in front of code that's supposed to be retry-safe end-to-end.
        let version = retry_on_admission_exhaustion(
            MAX_PAGED_FETCH_ATTEMPTS,
            MAX_PAGED_FETCH_BACKOFF,
            || async { self.0.version(true).await.map_err(CarbideCliError::from) },
        )
        .await?;
        let cap = version.runtime_config.unwrap_or_default().max_find_by_ids as usize;
        Ok(cap_chunk_size(page_size, cap))
    }

    pub(crate) async fn get_machine(&self, id: MachineId) -> CarbideCliResult<rpc::Machine> {
        let mut machines = self
            .0
            .find_machines_by_ids(::rpc::forge::MachinesByIdsRequest {
                machine_ids: vec![id],
                include_history: true,
            })
            .await?;

        if machines.machines.is_empty() {
            return Err(CarbideCliError::MachineNotFound(id));
        }

        let machine_details = machines.machines.remove(0);

        Ok(machine_details)
    }

    /// Gather one machine's boot-interface view across all four stores -- the
    /// owned interface rows, predictions, the explored endpoint default, and
    /// the retained post-deletion pairs -- plus the effective boot interface
    /// and a divergence flag. Read-only.
    pub(crate) async fn get_machine_boot_interfaces(
        &self,
        id: MachineId,
    ) -> CarbideCliResult<rpc::GetMachineBootInterfacesResponse> {
        Ok(self
            .0
            .get_machine_boot_interfaces(rpc::GetMachineBootInterfacesRequest {
                machine_id: Some(id),
            })
            .await?)
    }

    pub(crate) async fn get_all_machines(
        &self,
        request: rpc::MachineSearchConfig,
        page_size: usize,
    ) -> CarbideCliResult<rpc::MachineList> {
        let all_machine_ids = self.0.find_machine_ids(request).await?;
        let mut all_machines = rpc::MachineList {
            machines: Vec::with_capacity(all_machine_ids.machine_ids.len()),
        };

        stream::iter(
            all_machine_ids
                .machine_ids
                .chunks(self.effective_chunk_size(page_size).await?),
        )
        .map(|machine_ids| self.get_machines_by_ids(machine_ids))
        .buffered(PAGED_LIST_FETCH_CONCURRENCY)
        .try_for_each(|machines| {
            all_machines.machines.extend(machines.machines);
            futures::future::ok(())
        })
        .await?;

        Ok(all_machines)
    }

    pub(crate) async fn identify_uuid(&self, u: uuid::Uuid) -> CarbideCliResult<rpc::UuidType> {
        let request = rpc::IdentifyUuidRequest {
            uuid: Some(u.into()),
        };

        let uuid_details = match self.0.identify_uuid(request).await {
            Ok(m) => m,
            Err(status) if status.code() == tonic::Code::NotFound => {
                return Err(CarbideCliError::UuidNotFound);
            }
            Err(err) => {
                tracing::error!(error = %err, "identify_uuid error calling grpc identify_uuid");
                return Err(CarbideCliError::GenericError(err.to_string()));
            }
        };
        let object_type = match rpc::UuidType::try_from(uuid_details.object_type) {
            Ok(ot) => ot,
            Err(e) => {
                tracing::error!(
                    object_type = uuid_details.object_type,
                    error = %e,
                    "Invalid UUID type from Carbide API",
                );
                return Err(CarbideCliError::GenericError(e.to_string()));
            }
        };

        Ok(object_type)
    }

    pub(crate) async fn identify_mac(
        &self,
        mac_address: MacAddress,
    ) -> CarbideCliResult<(rpc::MacOwner, String)> {
        let request = rpc::IdentifyMacRequest {
            mac_address: mac_address.to_string(),
        };

        let mac_details = match self.0.identify_mac(request).await {
            Ok(m) => m,
            Err(status) if status.code() == tonic::Code::NotFound => {
                return Err(CarbideCliError::MacAddressNotFound);
            }
            Err(err) => {
                tracing::error!(error = %err, "identify_mac error calling grpc identify_mac");
                return Err(CarbideCliError::GenericError(err.to_string()));
            }
        };
        let object_type = match rpc::MacOwner::try_from(mac_details.object_type) {
            Ok(ot) => ot,
            Err(e) => {
                tracing::error!(
                    object_type = mac_details.object_type,
                    error = %e,
                    "Invalid machine owner from Carbide API",
                );
                return Err(CarbideCliError::GenericError(e.to_string()));
            }
        };

        Ok((object_type, mac_details.primary_key))
    }

    pub(crate) async fn identify_serial(
        &self,
        serial_number: String,
        exact: bool,
    ) -> CarbideCliResult<MachineId> {
        let serial_details = match self
            .0
            .identify_serial(IdentifySerialRequest {
                serial_number,
                exact,
            })
            .await
        {
            Ok(m) => m,
            Err(status) if status.code() == tonic::Code::NotFound => {
                return Err(CarbideCliError::SerialNumberNotFound);
            }
            Err(err) => {
                tracing::error!(error = %err, "identify_serial error calling grpc identify_serial");
                return Err(CarbideCliError::GenericError(err.to_string()));
            }
        };

        serial_details
            .machine_id
            .ok_or(CarbideCliError::GenericError(
                "Serial number found without associated machine ID".to_string(),
            ))
    }

    /// Resolves the full instance list matching a filter, via one id-listing
    /// RPC followed by concurrently-chunked `find_instances_by_ids` fetches.
    ///
    /// Each of those calls is retried individually on `RESOURCE_EXHAUSTED`
    /// (see [`retry_on_admission_exhaustion`]), rather than the whole method
    /// being wrapped by a caller-side retry. A rejection while fetching, say,
    /// the last of 20 chunks would otherwise burn the whole outer retry
    /// budget re-fetching all 20 chunks from scratch, discarding the 19 that
    /// already succeeded -- found via PR review at large-batch (`--label-key`)
    /// scale, where a late chunk landing in a saturated admission window was
    /// common.
    pub(crate) async fn get_all_instances(
        &self,
        tenant_org_id: Option<String>,
        vpc_id: Option<String>,
        label_key: Option<String>,
        label_value: Option<String>,
        instance_type_id: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::InstanceList> {
        let all_ids = self
            .get_instance_ids(
                tenant_org_id,
                vpc_id,
                label_key,
                label_value,
                instance_type_id,
            )
            .await?;
        let mut all_list = rpc::InstanceList {
            instances: Vec::with_capacity(all_ids.instance_ids.len()),
        };

        stream::iter(
            all_ids
                .instance_ids
                .chunks(self.effective_chunk_size(page_size).await?),
        )
        .map(|ids| {
            let ids = ids.to_vec();
            retry_on_admission_exhaustion(
                MAX_PAGED_FETCH_ATTEMPTS,
                MAX_PAGED_FETCH_BACKOFF,
                move || {
                    let ids = ids.clone();
                    async move {
                        self.0
                            .find_instances_by_ids(ids)
                            .await
                            .map_err(CarbideCliError::from)
                    }
                },
            )
        })
        .buffered(PAGED_LIST_FETCH_CONCURRENCY)
        .try_for_each(|list| {
            all_list.instances.extend(list.instances);
            futures::future::ok(())
        })
        .await?;

        Ok(all_list)
    }

    pub(crate) async fn get_one_instance(
        &self,
        instance_id: InstanceId,
    ) -> CarbideCliResult<rpc::InstanceList> {
        let instances = self.0.find_instances_by_ids(vec![instance_id]).await?;

        Ok(instances)
    }

    async fn get_instance_ids(
        &self,
        tenant_org_id: Option<String>,
        vpc_id: Option<String>,
        label_key: Option<String>,
        label_value: Option<String>,
        instance_type_id: Option<String>,
    ) -> CarbideCliResult<rpc::InstanceIdList> {
        let request = rpc::InstanceSearchFilter {
            tenant_org_id,
            vpc_id,
            instance_type_id,
            label: if label_key.is_none() && label_value.is_none() {
                None
            } else {
                Some(rpc::Label {
                    key: label_key.unwrap_or_default(),
                    value: label_value,
                })
            },
        };
        retry_on_admission_exhaustion(MAX_PAGED_FETCH_ATTEMPTS, MAX_PAGED_FETCH_BACKOFF, || {
            let request = request.clone();
            async move {
                self.0
                    .find_instance_ids(request)
                    .await
                    .map_err(CarbideCliError::from)
            }
        })
        .await
    }

    pub(crate) async fn get_all_racks(&self, page_size: usize) -> CarbideCliResult<rpc::RackList> {
        let all_ids = self.get_rack_ids().await?;
        let mut all_list = rpc::RackList {
            racks: Vec::with_capacity(all_ids.rack_ids.len()),
        };

        for ids in all_ids
            .rack_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.0.find_racks_by_ids(ids.to_vec()).await?;
            all_list.racks.extend(list.racks);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_rack(&self, rack_id: RackId) -> CarbideCliResult<rpc::RackList> {
        let racks = self.0.find_racks_by_ids(vec![rack_id]).await?;

        Ok(racks)
    }

    pub(crate) async fn get_rack_profile(
        &self,
        rack_id: RackId,
    ) -> CarbideCliResult<rpc::GetRackProfileResponse> {
        Ok(self
            .0
            .get_rack_profile(rpc::GetRackProfileRequest {
                rack_id: Some(rack_id),
            })
            .await?)
    }

    pub(crate) async fn list_rack_profiles(
        &self,
    ) -> CarbideCliResult<rpc::ListRackProfilesResponse> {
        Ok(self.0.list_rack_profiles().await?)
    }

    async fn get_rack_ids(&self) -> CarbideCliResult<rpc::RackIdList> {
        Ok(self
            .0
            .find_rack_ids(rpc::RackSearchFilter::default())
            .await?)
    }

    pub(crate) async fn get_all_switches(
        &self,
        filter: rpc::SwitchSearchFilter,
        page_size: usize,
    ) -> CarbideCliResult<rpc::SwitchList> {
        let all_ids = self.0.find_switch_ids(filter).await?;
        let mut all_list = rpc::SwitchList {
            switches: Vec::with_capacity(all_ids.ids.len()),
        };

        for ids in all_ids
            .ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self
                .0
                .find_switches_by_ids(rpc::SwitchesByIdsRequest {
                    switch_ids: ids.to_vec(),
                })
                .await?;
            all_list.switches.extend(list.switches);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_switch(
        &self,
        switch_id: SwitchId,
    ) -> CarbideCliResult<rpc::SwitchList> {
        Ok(self
            .0
            .find_switches_by_ids(rpc::SwitchesByIdsRequest {
                switch_ids: vec![switch_id],
            })
            .await?)
    }

    pub(crate) async fn get_all_power_shelves(
        &self,
        filter: rpc::PowerShelfSearchFilter,
        page_size: usize,
    ) -> CarbideCliResult<rpc::PowerShelfList> {
        let all_ids = self.0.find_power_shelf_ids(filter).await?;
        let mut all_list = rpc::PowerShelfList {
            power_shelves: Vec::with_capacity(all_ids.ids.len()),
        };

        for ids in all_ids
            .ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self
                .0
                .find_power_shelves_by_ids(rpc::PowerShelvesByIdsRequest {
                    power_shelf_ids: ids.to_vec(),
                })
                .await?;
            all_list.power_shelves.extend(list.power_shelves);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_power_shelf(
        &self,
        power_shelf_id: PowerShelfId,
    ) -> CarbideCliResult<rpc::PowerShelfList> {
        Ok(self
            .0
            .find_power_shelves_by_ids(rpc::PowerShelvesByIdsRequest {
                power_shelf_ids: vec![power_shelf_id],
            })
            .await?)
    }

    pub(crate) async fn get_all_segments(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::NetworkSegmentList> {
        let all_ids = self.get_segment_ids(tenant_org_id, name).await?;
        let mut all_list = rpc::NetworkSegmentList {
            network_segments: Vec::with_capacity(all_ids.network_segments_ids.len()),
        };

        for ids in all_ids
            .network_segments_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_segments_by_ids(ids).await?;
            all_list.network_segments.extend(list.network_segments);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_segment(
        &self,
        segment_id: NetworkSegmentId,
    ) -> CarbideCliResult<rpc::NetworkSegmentList> {
        let segments = self.get_segments_by_ids(&[segment_id]).await?;

        Ok(segments)
    }

    async fn get_segment_ids(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
    ) -> CarbideCliResult<rpc::NetworkSegmentIdList> {
        let request = rpc::NetworkSegmentSearchFilter {
            tenant_org_id,
            name,
        };
        Ok(self.0.find_network_segment_ids(request).await?)
    }

    pub(crate) async fn get_segments_by_ids(
        &self,
        network_segments_ids: &[NetworkSegmentId],
    ) -> CarbideCliResult<rpc::NetworkSegmentList> {
        let request = rpc::NetworkSegmentsByIdsRequest {
            network_segments_ids: network_segments_ids.to_vec(),
            // Request inline history for single-segment lookups so old servers (lacking the
            // FindNetworkSegmentStateHistories RPC) still populate the deprecated history field.
            include_history: network_segments_ids.len() == 1,
            include_num_free_ips: true,
        };
        Ok(self.0.find_network_segments_by_ids(request).await?)
    }

    pub(crate) async fn get_rack_state_history(
        &self,
        rack_id: RackId,
    ) -> CarbideCliResult<Vec<rpc::StateHistoryRecord>> {
        let mut result = self
            .0
            .find_rack_state_histories(rpc::RackStateHistoriesRequest {
                rack_ids: vec![rack_id.clone()],
            })
            .await?;

        Ok(result
            .histories
            .remove(&rack_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_switch_health_history(
        &self,
        switch_id: SwitchId,
    ) -> CarbideCliResult<Vec<rpc::HealthHistoryRecord>> {
        let mut result = self
            .0
            .find_switch_health_histories(rpc::SwitchHealthHistoriesRequest {
                switch_ids: vec![switch_id],
                start_time: None,
                end_time: None,
            })
            .await?;

        Ok(result
            .histories
            .remove(&switch_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_power_shelf_health_history(
        &self,
        power_shelf_id: PowerShelfId,
    ) -> CarbideCliResult<Vec<rpc::HealthHistoryRecord>> {
        let mut result = self
            .0
            .find_power_shelf_health_histories(rpc::PowerShelfHealthHistoriesRequest {
                power_shelf_ids: vec![power_shelf_id],
                start_time: None,
                end_time: None,
            })
            .await?;

        Ok(result
            .histories
            .remove(&power_shelf_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_rack_health_history(
        &self,
        rack_id: RackId,
    ) -> CarbideCliResult<Vec<rpc::HealthHistoryRecord>> {
        let mut result = self
            .0
            .find_rack_health_histories(rpc::RackHealthHistoriesRequest {
                rack_ids: vec![rack_id.clone()],
                start_time: None,
                end_time: None,
            })
            .await?;

        Ok(result
            .histories
            .remove(&rack_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_machine_health_history(
        &self,
        machine_id: MachineId,
    ) -> CarbideCliResult<Vec<rpc::HealthHistoryRecord>> {
        let mut result = self
            .0
            .find_machine_health_histories(rpc::MachineHealthHistoriesRequest {
                machine_ids: vec![machine_id],
                start_time: None,
                end_time: None,
            })
            .await?;

        Ok(result
            .histories
            .remove(&machine_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_segment_state_history(
        &self,
        segment_id: NetworkSegmentId,
    ) -> CarbideCliResult<Vec<rpc::StateHistoryRecord>> {
        let mut result = self
            .0
            .find_network_segment_state_histories(rpc::NetworkSegmentStateHistoriesRequest {
                network_segment_ids: vec![segment_id],
            })
            .await?;

        Ok(result
            .histories
            .remove(&segment_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    /// Fetches controller state history for a single VPC prefix.
    pub(crate) async fn get_vpc_prefix_state_history(
        &self,
        vpc_prefix_id: VpcPrefixId,
    ) -> CarbideCliResult<Vec<rpc::StateHistoryRecord>> {
        // Request the history through the generic state-history RPC.
        let mut result = self
            .0
            .find_vpc_prefix_state_histories(rpc::VpcPrefixStateHistoriesRequest {
                vpc_prefix_ids: vec![vpc_prefix_id],
            })
            .await?;

        // Return an empty list when the object has no recorded transitions yet.
        Ok(result
            .histories
            .remove(&vpc_prefix_id.to_string())
            .map(|h| h.records)
            .unwrap_or_default())
    }

    /// Fetches controller state history for a single SitePrefix.
    pub(crate) async fn get_site_prefix_state_history(
        &self,
        site_prefix_id: SitePrefixId,
    ) -> CarbideCliResult<Vec<rpc::StateHistoryRecord>> {
        let mut result = self
            .0
            .find_site_prefix_state_histories(rpc::SitePrefixStateHistoriesRequest {
                site_prefix_ids: vec![site_prefix_id],
            })
            .await?;

        Ok(result
            .histories
            .remove(&site_prefix_id.to_string())
            .map(|history| history.records)
            .unwrap_or_default())
    }

    pub(crate) async fn get_domains(
        &self,
        id: Option<::carbide_uuid::domain::DomainId>,
    ) -> CarbideCliResult<::rpc::protos::dns::DomainList> {
        let request = ::rpc::protos::dns::DomainSearchQuery { id, name: None };
        Ok(self.0.find_domain(request).await?)
    }

    pub(crate) async fn machine_insert_health_report_override(
        &self,
        id: &MachineId,
        report: ::rpc::health::HealthReport,
        replace: bool,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::InsertMachineHealthReportRequest {
            machine_id: Some(*id),
            health_report_entry: Some(rpc::HealthReportEntry {
                report: Some(report),
                mode: if replace {
                    rpc::HealthReportApplyMode::Replace
                } else {
                    rpc::HealthReportApplyMode::Merge
                } as i32,
            }),
        };
        match self.0.insert_machine_health_report(request.clone()).await {
            Ok(()) => Ok(()),
            Err(status) if maybe_unimplemented(&status) => {
                // Fall back to the deprecated alias for older API servers
                // that don't have the renamed RPC yet.
                #[allow(deprecated)]
                Ok(self.0.insert_health_report_override(request).await?)
            }
            Err(status) => Err(status.into()),
        }
    }

    pub(crate) async fn machine_list_health_reports(
        &self,
        machine_id: MachineId,
    ) -> CarbideCliResult<rpc::ListHealthReportResponse> {
        match self.0.list_machine_health_reports(machine_id).await {
            Ok(response) => Ok(response),
            Err(status) if maybe_unimplemented(&status) => {
                // Fall back to the deprecated alias for older API servers.
                #[allow(deprecated)]
                Ok(self.0.list_health_report_overrides(machine_id).await?)
            }
            Err(status) => Err(status.into()),
        }
    }

    pub(crate) async fn machine_remove_health_report(
        &self,
        machine_id: MachineId,
        source: String,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::RemoveMachineHealthReportRequest {
            machine_id: Some(machine_id),
            source,
        };
        match self.0.remove_machine_health_report(request.clone()).await {
            Ok(()) => Ok(()),
            Err(status) if maybe_unimplemented(&status) => {
                // Fall back to the deprecated alias for older API servers.
                #[allow(deprecated)]
                Ok(self.0.remove_health_report_override(request).await?)
            }
            Err(status) => Err(status.into()),
        }
    }

    pub(crate) async fn admin_power_control(
        &self,
        bmc_endpoint_request: Option<BmcEndpointRequest>,
        machine_id: Option<String>,
        action: ::rpc::forge::admin_power_control_request::SystemPowerControl,
    ) -> CarbideCliResult<rpc::AdminPowerControlResponse> {
        let request = rpc::AdminPowerControlRequest {
            bmc_endpoint_request,
            machine_id,
            action: action.into(),
        };
        Ok(self.0.admin_power_control(request).await?)
    }

    pub(crate) async fn get_all_machines_interfaces(
        &self,
        id: Option<MachineInterfaceId>,
    ) -> CarbideCliResult<rpc::InterfaceList> {
        let request = rpc::InterfaceSearchQuery { id, ip: None };
        Ok(self.0.find_interfaces(request).await?)
    }

    pub(crate) async fn get_site_exploration_report(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<::rpc::site_explorer::SiteExplorationReport> {
        let last_run = self.get_site_explorer_last_run().await?;
        // grab endpoints
        let endpoint_ids = match self.0.find_explored_endpoint_ids().await {
            Ok(endpoint_ids) => endpoint_ids,
            Err(status) => {
                return if maybe_unimplemented(&status) {
                    Ok(self.0.get_site_exploration_report().await?)
                } else {
                    Err(status.into())
                };
            }
        };
        let mut all_endpoints = ::rpc::site_explorer::ExploredEndpointList {
            endpoints: Vec::with_capacity(endpoint_ids.endpoint_ids.len()),
        };

        stream::iter(
            endpoint_ids
                .endpoint_ids
                .chunks(self.effective_chunk_size(page_size).await?),
        )
        .map(|ids| self.get_explored_endpoints_by_ids(ids))
        .buffered(PAGED_LIST_FETCH_CONCURRENCY)
        .try_for_each(|list| {
            all_endpoints.endpoints.extend(list.endpoints);
            futures::future::ok(())
        })
        .await?;

        // grab managed hosts
        let all_hosts = self.get_all_explored_managed_hosts(page_size).await?;

        Ok(::rpc::site_explorer::SiteExplorationReport {
            endpoints: all_endpoints.endpoints,
            managed_hosts: all_hosts,
            last_run,
        })
    }

    async fn get_site_explorer_last_run(
        &self,
    ) -> CarbideCliResult<Option<::rpc::site_explorer::SiteExplorerLastRun>> {
        match self.0.get_site_explorer_last_run().await {
            Ok(response) => Ok(response.last_run),
            Err(status) if maybe_unimplemented(&status) => Ok(None),
            Err(status) => Err(status.into()),
        }
    }

    pub(crate) async fn get_explored_endpoints_by_ids(
        &self,
        endpoint_ids: &[String],
    ) -> CarbideCliResult<::rpc::site_explorer::ExploredEndpointList> {
        let request = ::rpc::site_explorer::ExploredEndpointsByIdsRequest {
            endpoint_ids: endpoint_ids.to_vec(),
        };
        Ok(self.0.find_explored_endpoints_by_ids(request).await?)
    }

    pub(crate) async fn get_all_explored_managed_hosts(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<Vec<::rpc::site_explorer::ExploredManagedHost>> {
        let host_ids = match self.0.find_explored_managed_host_ids().await {
            Ok(host_ids) => host_ids,
            Err(status) if maybe_unimplemented(&status) => {
                let hosts = self.0.get_site_exploration_report().await?.managed_hosts;
                return Ok(hosts);
            }
            Err(e) => return Err(e.into()),
        };
        let mut all_hosts = ::rpc::site_explorer::ExploredManagedHostList {
            managed_hosts: Vec::with_capacity(host_ids.host_ids.len()),
        };

        stream::iter(
            host_ids
                .host_ids
                .chunks(self.effective_chunk_size(page_size).await?),
        )
        .map(|ids| self.0.find_explored_managed_hosts_by_ids(ids))
        .buffered(PAGED_LIST_FETCH_CONCURRENCY)
        .try_for_each(|list| {
            all_hosts.managed_hosts.extend(list.managed_hosts);
            futures::future::ok(())
        })
        .await?;

        Ok(all_hosts.managed_hosts)
    }

    pub(crate) async fn get_all_explored_mlx_devices(
        &self,
        page_size: usize,
        host: Option<String>,
    ) -> CarbideCliResult<Vec<::rpc::site_explorer::ExploredMlxDevice>> {
        // A specific host short-circuits the id listing; otherwise list every host
        // BMC carrying BlueField devices and page through them.
        let host_ids: Vec<String> = match host {
            Some(host) => vec![host],
            None => self.0.find_explored_mlx_device_host_ids().await?.host_ids,
        };

        let mut all = ::rpc::site_explorer::ExploredMlxDeviceList {
            devices: Vec::with_capacity(host_ids.len()),
        };
        stream::iter(host_ids.chunks(self.effective_chunk_size(page_size).await?))
            .map(|ids| self.0.find_explored_mlx_devices_by_ids(ids))
            .buffered(PAGED_LIST_FETCH_CONCURRENCY)
            .try_for_each(|list| {
                all.devices.extend(list.devices);
                futures::future::ok(())
            })
            .await?;

        Ok(all.devices)
    }

    pub(crate) async fn get_machines_by_ids(
        &self,
        machine_ids: &[impl MachineIdSubtypeTrait],
    ) -> CarbideCliResult<rpc::MachineList> {
        let request = ::rpc::forge::MachinesByIdsRequest {
            machine_ids: machine_ids.iter().copied().map(Into::into).collect(),
            ..Default::default()
        };
        Ok(self.0.find_machines_by_ids(request).await?)
    }

    pub(crate) async fn set_dynamic_config(
        &self,
        feature: rpc::ConfigSetting,
        value: String,
        expiry: Option<String>,
    ) -> CarbideCliResult<()> {
        let request = rpc::SetDynamicConfigRequest {
            setting: feature.into(),
            value,
            expiry,
        };
        Ok(self.0.set_dynamic_config(request).await?)
    }

    /// Partially updates an expected machine: merges CLI-provided fields with the current API
    /// record, then calls `update_expected_machine`. When `bmc_ip_address` is supplied, the server
    /// runs the same static-interface reconciliation as a full RPC update.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn patch_expected_machine(
        &self,
        bmc_mac_address: Option<MacAddress>,
        id: Option<String>,
        bmc_username: Option<String>,
        bmc_password: Option<String>,
        chassis_serial_number: Option<String>,
        fallback_dpu_serial_numbers: Option<Vec<String>>,
        meta_name: Option<String>,
        meta_description: Option<String>,
        labels: Option<Vec<String>>,
        sku_id: Option<String>,
        rack_id: Option<RackId>,
        default_pause_ingestion_and_poweron: Option<bool>,
        dpf_enabled: Option<bool>,
        bmc_ip_address: Option<String>,
        bmc_retain_credentials: Option<bool>,
        dpu_policy: Option<HostDpuPolicy>,
        bmc_ip_allocation: Option<::rpc::forge::BmcIpAllocationType>,
        host_lifecycle_profile: Option<::rpc::forge::HostLifecycleProfile>,
        interfaces: Option<String>,
    ) -> Result<(), CarbideCliError> {
        let get_req = match (bmc_mac_address, id) {
            (Some(_), Some(_)) => {
                return Err(CarbideCliError::ChooseOneError("--bmc-mac-address", "--id"));
            }
            (None, None) => {
                return Err(CarbideCliError::RequireOneError(
                    "--bmc-mac-address",
                    "--id",
                ));
            }
            (_, Some(id)) => ::rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: String::new(),
                id: Some(::rpc::common::Uuid { value: id }),
            },
            (Some(mac), None) => ::rpc::forge::ExpectedMachineRequest {
                bmc_mac_address: mac.to_string(),
                id: None,
            },
        };
        let expected_machine = self.0.get_expected_machine(get_req).await?;
        let mac_str = bmc_mac_address
            .map(|m| m.to_string())
            .unwrap_or(expected_machine.bmc_mac_address.clone());
        let parsed_interfaces = interfaces
            .map(|s| serde_json::from_str::<Vec<rpc::ExpectedInterface>>(&s))
            .transpose()?;
        let replace_interfaces = parsed_interfaces.is_some();
        let legacy_bmc_fields = legacy_bmc_patch_fields(
            &expected_machine,
            bmc_ip_address,
            bmc_ip_allocation,
            parsed_interfaces.as_deref(),
        );

        // Merge metadata fields individually
        let merged_metadata =
            if meta_name.is_some() || meta_description.is_some() || labels.is_some() {
                let existing = expected_machine.metadata.unwrap_or_default();

                // Convert labels to the proto format
                let merged_labels = if let Some(label_list) = labels {
                    let mut proto_labels = Vec::new();
                    for label in label_list {
                        let proto_label = match label.split_once(':') {
                            Some((k, v)) => ::rpc::forge::Label {
                                key: k.trim().to_string(),
                                value: Some(v.trim().to_string()),
                            },
                            None => ::rpc::forge::Label {
                                key: label.trim().to_string(),
                                value: None,
                            },
                        };
                        proto_labels.push(proto_label);
                    }
                    proto_labels
                } else {
                    existing.labels
                };

                Some(::rpc::forge::Metadata {
                    name: meta_name.unwrap_or(existing.name),
                    description: meta_description.unwrap_or(existing.description),
                    labels: merged_labels,
                })
            } else {
                expected_machine.metadata
            };

        let request = rpc::ExpectedMachine {
            bmc_mac_address: mac_str,
            bmc_username: bmc_username.unwrap_or(expected_machine.bmc_username),
            bmc_password: bmc_password.unwrap_or(expected_machine.bmc_password),
            chassis_serial_number: chassis_serial_number
                .unwrap_or(expected_machine.chassis_serial_number),
            fallback_dpu_serial_numbers: fallback_dpu_serial_numbers
                .unwrap_or(expected_machine.fallback_dpu_serial_numbers),
            metadata: merged_metadata,
            sku_id: sku_id.or(expected_machine.sku_id),
            id: expected_machine.id,
            host_nics: parsed_interfaces.unwrap_or(expected_machine.host_nics),
            rack_id: rack_id.or(expected_machine.rack_id),
            default_pause_ingestion_and_poweron: default_pause_ingestion_and_poweron
                .or(expected_machine.default_pause_ingestion_and_poweron),
            #[allow(deprecated)]
            dpf_enabled: dpf_enabled.unwrap_or(true),
            is_dpf_enabled: dpf_enabled,
            bmc_ip_address: legacy_bmc_fields.bmc_ip_address,
            bmc_retain_credentials: bmc_retain_credentials
                .or(expected_machine.bmc_retain_credentials),
            dpu_mode: dpu_policy
                .map(|policy| ::rpc::forge::DpuMode::from(policy) as i32)
                .or(expected_machine.dpu_mode),
            bmc_ip_allocation: legacy_bmc_fields.bmc_ip_allocation,
            replace_host_nics: replace_interfaces,
            host_lifecycle_profile: host_lifecycle_profile
                .or(expected_machine.host_lifecycle_profile),
        };

        Ok(self.0.update_expected_machine(request).await?)
    }

    /// Replaces the entire expected-machine table from JSON.
    pub(crate) async fn replace_all_expected_machines(
        &self,
        expected_machine_list: Vec<ExpectedMachineJson>,
    ) -> Result<(), CarbideCliError> {
        let request = rpc::ExpectedMachineList {
            expected_machines: expected_machine_list
                .into_iter()
                .map(|machine| rpc::ExpectedMachine {
                    dpu_mode: machine
                        .dpu_policy()
                        .map(|policy| ::rpc::forge::DpuMode::from(policy) as i32),
                    id: machine.id.map(|s| ::rpc::common::Uuid { value: s }),
                    bmc_mac_address: machine.bmc_mac_address.to_string(),
                    bmc_username: machine.bmc_username,
                    bmc_password: machine.bmc_password,
                    chassis_serial_number: machine.chassis_serial_number,
                    fallback_dpu_serial_numbers: machine
                        .fallback_dpu_serial_numbers
                        .unwrap_or_default(),
                    metadata: machine.metadata,
                    sku_id: machine.sku_id,
                    host_nics: machine.interfaces.unwrap_or_default(),
                    rack_id: machine.rack_id,
                    default_pause_ingestion_and_poweron: machine
                        .default_pause_ingestion_and_poweron,
                    #[allow(deprecated)]
                    dpf_enabled: machine.dpf_enabled.unwrap_or(true),
                    is_dpf_enabled: machine.dpf_enabled,
                    bmc_ip_address: machine.bmc_ip_address,
                    bmc_retain_credentials: machine.bmc_retain_credentials,
                    bmc_ip_allocation: machine.bmc_ip_allocation.map(|m| m as i32),
                    // replace-all is authoritative even when the JSON field is
                    // omitted and therefore resolves to an empty list.
                    replace_host_nics: true,
                    host_lifecycle_profile: machine.host_lifecycle_profile.map(|hlp| {
                        ::rpc::forge::HostLifecycleProfile {
                            disable_lockdown: hlp.disable_lockdown,
                        }
                    }),
                })
                .collect(),
        };

        Ok(self.0.replace_all_expected_machines(request).await?)
    }

    pub(crate) async fn replace_all_expected_power_shelves(
        &self,
        expected_power_shelf_list: Vec<crate::expected_power_shelf::common::ExpectedPowerShelfJson>,
    ) -> Result<(), CarbideCliError> {
        let request = rpc::ExpectedPowerShelfList {
            expected_power_shelves: expected_power_shelf_list
                .into_iter()
                .map(|power_shelf| rpc::ExpectedPowerShelf {
                    expected_power_shelf_id: None,
                    bmc_mac_address: power_shelf.bmc_mac_address.to_string(),
                    bmc_username: power_shelf.bmc_username,
                    bmc_password: power_shelf.bmc_password,
                    shelf_serial_number: power_shelf.shelf_serial_number,
                    bmc_ip_address: power_shelf
                        .bmc_ip_address
                        .map(|ip| ip.to_string())
                        .unwrap_or_default(),
                    metadata: power_shelf.metadata,
                    rack_id: power_shelf.rack_id,
                    bmc_retain_credentials: power_shelf.bmc_retain_credentials,
                })
                .collect(),
        };
        self.0
            .replace_all_expected_power_shelves(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    pub(crate) async fn replace_all_expected_switches(
        &self,
        expected_switch_list: Vec<crate::expected_switch::common::ExpectedSwitchJson>,
    ) -> Result<(), CarbideCliError> {
        let request = rpc::ExpectedSwitchList {
            expected_switches: expected_switch_list
                .into_iter()
                .map(|switch| rpc::ExpectedSwitch {
                    expected_switch_id: None,
                    bmc_mac_address: switch.bmc_mac_address.to_string(),
                    bmc_username: switch.bmc_username,
                    bmc_password: switch.bmc_password,
                    switch_serial_number: switch.switch_serial_number,
                    nvos_mac_addresses: switch
                        .nvos_mac_addresses
                        .iter()
                        .map(|m| m.to_string())
                        .collect(),
                    nvos_username: switch.nvos_username,
                    nvos_password: switch.nvos_password,
                    bmc_ip_address: switch
                        .bmc_ip_address
                        .map(|ip| ip.to_string())
                        .unwrap_or_default(),
                    nvos_ip_address: switch.nvos_ip_address.map(|ip| ip.to_string()),
                    metadata: switch.metadata,
                    rack_id: switch.rack_id,
                    bmc_retain_credentials: switch.bmc_retain_credentials,
                })
                .collect(),
        };
        self.0
            .replace_all_expected_switches(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    pub(crate) async fn get_all_vpcs(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        page_size: usize,
        label_key: Option<String>,
        label_value: Option<String>,
    ) -> CarbideCliResult<rpc::VpcList> {
        let all_ids = self
            .get_vpc_ids(tenant_org_id, name, label_key, label_value)
            .await?;
        let mut all_list = rpc::VpcList {
            vpcs: Vec::with_capacity(all_ids.vpc_ids.len()),
        };

        for ids in all_ids
            .vpc_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.0.find_vpcs_by_ids(ids).await?;
            all_list.vpcs.extend(list.vpcs);
        }

        Ok(all_list)
    }

    // Get all the DPA interfaces and return the vector of DPA interfaces
    pub(crate) async fn get_all_dpas(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<rpc::DpaInterfaceList> {
        let all_ids = self.get_dpa_ids().await?;
        let mut all_list = rpc::DpaInterfaceList {
            interfaces: Vec::with_capacity(all_ids.ids.len()),
        };

        let include_history = all_ids.ids.len() == 1;

        for ids in all_ids
            .ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let request = rpc::DpaInterfacesByIdsRequest {
                ids: ids.to_vec(),
                include_history,
            };

            let list = self.0.find_dpa_interfaces_by_ids(request).await?;
            all_list.interfaces.extend(list.interfaces);
        }

        Ok(all_list)
    }

    // Given an DPA interface ID, fetch it from Carbide and return it
    pub(crate) async fn get_one_dpa(
        &self,
        dpa_id: DpaInterfaceId,
    ) -> CarbideCliResult<rpc::DpaInterfaceList> {
        let request = rpc::DpaInterfacesByIdsRequest {
            ids: vec![dpa_id],
            include_history: true,
        };

        Ok(self.0.find_dpa_interfaces_by_ids(request).await?)
    }

    pub(crate) async fn get_vpc_by_name(&self, name: &str) -> CarbideCliResult<rpc::VpcList> {
        let vpc_ids = self
            .0
            .find_vpc_ids(VpcSearchFilter {
                label: None,
                tenant_org_id: None,
                name: Some(name.to_string()),
            })
            .await?
            .vpc_ids;

        Ok(if vpc_ids.is_empty() {
            rpc::VpcList { vpcs: vec![] }
        } else {
            self.0
                .find_vpcs_by_ids(VpcsByIdsRequest { vpc_ids })
                .await?
        })
    }

    pub(crate) async fn create_vpc(&self, name: &str, vpc_id: VpcId) -> CarbideCliResult<rpc::Vpc> {
        let vpc = match self
            .0
            .create_vpc(VpcCreationRequest {
                vni: None,
                routing_profile_type: None,
                routing_profile_overrides: None,
                power_resource_group: None,
                slaac_enabled: None,
                tenant_organization_id: "devenv_test_org".to_string(),
                tenant_keyset_id: None,
                network_virtualization_type: Some(
                    VpcVirtualizationType::EthernetVirtualizer.into(),
                ),
                id: Some(vpc_id),
                metadata: Some(rpc::Metadata {
                    name: name.to_string(),
                    description: "test vpc".to_string(),
                    labels: vec![],
                }),
                network_security_group_id: None,
                default_nvlink_logical_partition_id: None,
            })
            .await
        {
            Ok(vpc) => vpc,
            Err(e) => return Err(e.into()),
        };

        Ok(vpc)
    }

    pub(crate) async fn create_network_segment(
        &self,
        id: NetworkSegmentId,
        vpc_id: Option<VpcId>,
        name: String,
        prefix: String,
        gateway: Option<String>,
    ) -> CarbideCliResult<NetworkSegment> {
        let request = NetworkSegmentCreationRequest {
            vpc_id,
            name,
            subdomain_id: None,
            mtu: Some(9000),
            prefixes: vec![NetworkPrefix {
                id: None,
                prefix,
                gateway,
                reserve_first: 0,
                free_ip_count: 1,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            segment_type: NetworkSegmentType::Tenant as i32,
            id: Some(id),
            infer_slaac_eui64_addresses: false,
        };
        Ok(self.0.create_network_segment(request).await?)
    }

    pub(crate) async fn create_flat_vpc(
        &self,
        name: &str,
        vpc_id: VpcId,
    ) -> CarbideCliResult<rpc::Vpc> {
        Ok(self
            .0
            .create_vpc(VpcCreationRequest {
                vni: None,
                routing_profile_type: None,
                routing_profile_overrides: None,
                power_resource_group: None,
                slaac_enabled: None,
                tenant_organization_id: "devenv_test_org".to_string(),
                tenant_keyset_id: None,
                network_virtualization_type: Some(VpcVirtualizationType::Flat.into()),
                id: Some(vpc_id),
                metadata: Some(rpc::Metadata {
                    name: name.to_string(),
                    description: "Flat VPC for zero-DPU HostInband segments".to_string(),
                    labels: vec![],
                }),
                network_security_group_id: None,
                default_nvlink_logical_partition_id: None,
            })
            .await?)
    }

    pub(crate) async fn create_host_inband_segment(
        &self,
        id: NetworkSegmentId,
        vpc_id: VpcId,
        name: String,
        prefix: String,
        gateway: Option<String>,
        reserve_first: i32,
    ) -> CarbideCliResult<NetworkSegment> {
        // Without a subdomain_id link the machine_dhcp_records view's inner
        // join on `domains` drops every interface created on this segment,
        // and Kea can't issue OFFERs. Require exactly one domain rather than
        // silently creating a segment that cannot DHCP or binding to an
        // arbitrary domain when several exist.
        let mut domain_ids = self
            .get_domains(None)
            .await?
            .domains
            .into_iter()
            .filter_map(|d| d.id);
        let subdomain_id = match (domain_ids.next(), domain_ids.next()) {
            (Some(id), None) => Some(id),
            (None, _) => {
                return Err(CarbideCliError::GenericError(
                    "no domain available for HostInband segment, create a domain first".to_string(),
                ));
            }
            (Some(_), Some(_)) => {
                return Err(CarbideCliError::GenericError(
                    "multiple domains found, cannot pick one for the HostInband segment"
                        .to_string(),
                ));
            }
        };
        let request = NetworkSegmentCreationRequest {
            vpc_id: Some(vpc_id),
            name,
            subdomain_id,
            mtu: Some(1500),
            prefixes: vec![NetworkPrefix {
                id: None,
                prefix,
                gateway,
                reserve_first,
                // computed by the server, ignored on create
                free_ip_count: 1,
                svi_ip: None,
                free_ip_count_v2: None,
                free_ip_count_saturated: false,
            }],
            segment_type: NetworkSegmentType::HostInband as i32,
            id: Some(id),
            infer_slaac_eui64_addresses: false,
        };
        Ok(self.0.create_network_segment(request).await?)
    }

    // Fetch from Carbide and return a vector of Dpa interface IDs
    async fn get_dpa_ids(&self) -> CarbideCliResult<rpc::DpaInterfaceIdList> {
        Ok(self.0.get_all_dpa_interface_ids().await?)
    }

    async fn get_vpc_ids(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        label_key: Option<String>,
        label_value: Option<String>,
    ) -> CarbideCliResult<rpc::VpcIdList> {
        let request = rpc::VpcSearchFilter {
            tenant_org_id,
            name,
            label: if label_key.is_none() && label_value.is_none() {
                None
            } else {
                Some(rpc::Label {
                    key: label_key.unwrap_or_default(),
                    value: label_value,
                })
            },
        };
        Ok(self.0.find_vpc_ids(request).await?)
    }

    /// set_vpc_network_virtualization_type sends out a `VpcUpdateVirtualizationRequest`
    /// to the API, with the purpose of being able to modify the underlying
    /// VpcVirtualizationType (or NetworkVirtualizationType) of the VPC. This will
    /// return an error if there are configured instances in the VPC (you can only
    /// do this with an empty VPC).
    pub(crate) async fn set_vpc_network_virtualization_type(
        &self,
        vpc: rpc::Vpc,
        virtualizer: VpcVirtualizationType,
    ) -> CarbideCliResult<()> {
        let request = rpc::VpcUpdateVirtualizationRequest {
            id: vpc.id,
            if_version_match: None,
            network_virtualization_type: Some(virtualizer.into()),
        };
        self.0.update_vpc_virtualization(request).await?;

        Ok(())
    }

    pub(crate) async fn get_all_ib_partitions(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::IbPartitionList> {
        let all_ids = self.get_ib_partition_ids(tenant_org_id, name).await?;
        let mut all_list = rpc::IbPartitionList {
            ib_partitions: Vec::with_capacity(all_ids.ib_partition_ids.len()),
        };

        for ids in all_ids
            .ib_partition_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_ib_partitions_by_ids(ids).await?;
            all_list.ib_partitions.extend(list.ib_partitions);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_all_spx_partitions(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::SpxPartitionList> {
        let all_ids = self.get_spx_partition_ids(tenant_org_id, name).await?;
        let mut all_list = rpc::SpxPartitionList {
            spx_partitions: Vec::with_capacity(all_ids.spx_partition_ids.len()),
        };

        for ids in all_ids
            .spx_partition_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_spx_partitions_by_ids(ids).await?;
            all_list.spx_partitions.extend(list.spx_partitions);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_spx_partition(
        &self,
        spx_partition_id: SpxPartitionId,
    ) -> CarbideCliResult<rpc::SpxPartitionList> {
        let partitions = self.get_spx_partitions_by_ids(&[spx_partition_id]).await?;
        Ok(partitions)
    }

    pub(crate) async fn get_one_ib_partition(
        &self,
        ib_partition_id: IBPartitionId,
    ) -> CarbideCliResult<rpc::IbPartitionList> {
        let partitions = self.get_ib_partitions_by_ids(&[ib_partition_id]).await?;

        Ok(partitions)
    }

    async fn get_ib_partition_ids(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
    ) -> CarbideCliResult<rpc::IbPartitionIdList> {
        let request = rpc::IbPartitionSearchFilter {
            tenant_org_id,
            name,
        };
        Ok(self.0.find_ib_partition_ids(request).await?)
    }

    async fn get_spx_partition_ids(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
    ) -> CarbideCliResult<rpc::SpxPartitionIdList> {
        let request = SpxPartitionSearchFilter {
            tenant_org_id,
            name,
            label: None,
        };
        Ok(self.0.find_spx_partition_ids(request).await?)
    }

    async fn get_ib_partitions_by_ids(
        &self,
        ids: &[IBPartitionId],
    ) -> CarbideCliResult<rpc::IbPartitionList> {
        let request = rpc::IbPartitionsByIdsRequest {
            ib_partition_ids: Vec::from(ids),
            include_history: ids.len() == 1,
        };
        Ok(self.0.find_ib_partitions_by_ids(request).await?)
    }

    async fn get_spx_partitions_by_ids(
        &self,
        ids: &[SpxPartitionId],
    ) -> CarbideCliResult<rpc::SpxPartitionList> {
        let request = rpc::SpxPartitionsByIdsRequest {
            spx_partition_ids: Vec::from(ids),
        };
        Ok(self.0.find_spx_partitions_by_ids(request).await?)
    }

    pub(crate) async fn get_all_keysets(
        &self,
        tenant_org_id: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::TenantKeySetList> {
        let all_ids = self.get_keyset_ids(tenant_org_id).await?;
        let mut all_list = rpc::TenantKeySetList {
            keyset: Vec::with_capacity(all_ids.keyset_ids.len()),
        };

        for ids in all_ids
            .keyset_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_keysets_by_ids(ids).await?;
            all_list.keyset.extend(list.keyset);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_keyset(
        &self,
        keyset_id: rpc::TenantKeysetIdentifier,
    ) -> CarbideCliResult<rpc::TenantKeySetList> {
        let keysets = self
            .get_keysets_by_ids(std::slice::from_ref(&keyset_id))
            .await?;

        Ok(keysets)
    }

    async fn get_keyset_ids(
        &self,
        tenant_org_id: Option<String>,
    ) -> CarbideCliResult<rpc::TenantKeysetIdList> {
        let request = rpc::TenantKeysetSearchFilter { tenant_org_id };
        Ok(self.0.find_tenant_keyset_ids(request).await?)
    }

    async fn get_keysets_by_ids(
        &self,
        identifiers: &[rpc::TenantKeysetIdentifier],
    ) -> CarbideCliResult<rpc::TenantKeySetList> {
        let request = rpc::TenantKeysetsByIdsRequest {
            keyset_ids: Vec::from(identifiers),
            include_key_data: true,
        };
        Ok(self.0.find_tenant_keysets_by_ids(request).await?)
    }

    pub(crate) async fn machine_set_auto_update(
        &self,
        req: MachineAutoupdate,
    ) -> CarbideCliResult<::rpc::forge::MachineSetAutoUpdateResponse> {
        let action = if req.enable {
            ::rpc::forge::machine_set_auto_update_request::SetAutoupdateAction::Enable
        } else if req.disable {
            ::rpc::forge::machine_set_auto_update_request::SetAutoupdateAction::Disable
        } else {
            ::rpc::forge::machine_set_auto_update_request::SetAutoupdateAction::Clear
        };
        let request = ::rpc::forge::MachineSetAutoUpdateRequest {
            machine_id: Some(req.machine),
            action: action.into(),
        };
        Ok(self.0.machine_set_auto_update(request).await?)
    }

    async fn get_subnet_ids_for_names(
        &self,
        subnets: &Vec<String>,
    ) -> CarbideCliResult<Vec<NetworkSegmentId>> {
        // find all the segment ids for the specified subnets.
        let mut network_segment_ids = Vec::default();
        for network_segment_name in subnets {
            let segment_request = rpc::NetworkSegmentSearchFilter {
                name: Some(network_segment_name.clone()),
                tenant_org_id: None,
            };

            match self.0.find_network_segment_ids(segment_request).await {
                Ok(response) => {
                    network_segment_ids.extend_from_slice(&response.network_segments_ids);
                }

                Err(e) => {
                    return Err(CarbideCliError::GenericError(format!(
                        "network segment: {network_segment_name} retrieval error {e}"
                    )));
                }
            }
        }

        Ok(network_segment_ids)
    }

    /// Build an InstanceAllocationRequest from CLI args and machine info.
    #[allow(deprecated)]
    pub(crate) async fn build_instance_request(
        &self,
        machine: Machine,
        allocate_instance: &AllocateInstance,
        instance_name: &str,
        modified_by: Option<String>,
    ) -> CarbideCliResult<rpc::InstanceAllocationRequest> {
        let mut vf_function_id = 0;
        let (interface_configs, tenant_org, vpc_id) = if let Some(vpc_id) =
            allocate_instance.flat_vpc_id
        {
            if !allocate_instance.subnet.is_empty()
                || !allocate_instance.vpc_prefix_id.is_empty()
                || !allocate_instance.vf_vpc_prefix_id.is_empty()
                || !allocate_instance.vf_subnet.is_empty()
                || !allocate_instance.ip_address.is_empty()
                || !allocate_instance.vf_ip_address.is_empty()
                || !allocate_instance.ipv6_vpc_prefix_id.is_empty()
                || !allocate_instance.ipv6_vf_prefix_id.is_empty()
                || !allocate_instance.ipv6_ip_address.is_empty()
                || !allocate_instance.ipv6_vf_ip_address.is_empty()
            {
                return Err(CarbideCliError::GenericError(
                    "--flat-vpc-id cannot be combined with explicit interface selectors"
                        .to_string(),
                ));
            }

            let vpc = self
                .0
                .find_vpcs_by_ids(VpcsByIdsRequest {
                    vpc_ids: vec![vpc_id],
                })
                .await?
                .vpcs
                .into_iter()
                .next()
                .ok_or_else(|| {
                    CarbideCliError::GenericError(format!("VPC {vpc_id} was not found"))
                })?;

            let network_virtualization_type = vpc
                .config
                .as_ref()
                .and_then(|config| config.network_virtualization_type)
                .and_then(|value| VpcVirtualizationType::try_from(value).ok())
                .unwrap_or_default();
            let VpcVirtualizationType::Flat = network_virtualization_type else {
                return Err(CarbideCliError::GenericError(format!(
                    "VPC {} is not a flat VPC, is of type {}",
                    vpc_id,
                    network_virtualization_type.as_str_name()
                )));
            };

            (
                Vec::new(),
                vpc.config
                    .as_ref()
                    .map(|c| c.tenant_organization_id.clone())
                    .ok_or_else(|| {
                        CarbideCliError::GenericError("VPC has no organization ID".to_string())
                    })?,
                Some(vpc_id),
            )
        } else if !allocate_instance.subnet.is_empty() {
            if !allocate_instance.vf_vpc_prefix_id.is_empty() {
                return Err(CarbideCliError::GenericError(
                    "Cannot use vf_vpc_prefix_id with subnet".to_string(),
                ));
            }
            let pf_network_segment_ids = self
                .get_subnet_ids_for_names(&allocate_instance.subnet)
                .await?;
            if pf_network_segment_ids.is_empty() {
                return Err(CarbideCliError::GenericError(
                    "no network segments found.".to_string(),
                ));
            }
            let vf_network_segment_ids = self
                .get_subnet_ids_for_names(&allocate_instance.vf_subnet)
                .await?;
            let vfs_per_pf = if vf_network_segment_ids.len() < pf_network_segment_ids.len() {
                1
            } else {
                vf_network_segment_ids.len() / pf_network_segment_ids.len()
            };
            tracing::debug!(vfs_per_pf, "VFs per PF",);

            let mut next_device_instance = HashMap::new();

            let Some(interfaces) = machine.discovery_info.map(|di| di.network_interfaces) else {
                return Err(CarbideCliError::GenericError(format!(
                    "no interface information for machine: {}",
                    machine.id.unwrap_or_default()
                )));
            };

            let mut interface_iter = interfaces.iter().filter(|iface| {
                iface
                    .pci_properties
                    .as_ref()
                    .map(|pci| &pci.vendor)
                    .is_some_and(|v| v.to_ascii_lowercase().contains("mellanox"))
            });
            let mut interface_config = Vec::default();
            let mut vf_chunk_iter = vf_network_segment_ids.chunks(vfs_per_pf);

            for network_segment_id in pf_network_segment_ids {
                let device = interface_iter
                    .next()
                    .ok_or(CarbideCliError::GenericError(
                        "Insufficient interfaces for selected machine".to_string(),
                    ))?
                    .pci_properties
                    .as_ref()
                    .map(|pci| pci.device.as_str());

                let Some(device) = device else {
                    continue;
                };

                let device_instance = *next_device_instance
                    .entry(device)
                    .and_modify(|i| *i += 1)
                    .or_insert(0) as u32;

                interface_config.push(rpc::InstanceInterfaceConfig {
                    function_type: rpc::InterfaceFunctionType::Physical as i32,
                    network_segment_id: Some(network_segment_id), // to support legacy.
                    network_details: Some(NetworkDetails::SegmentId(network_segment_id)),
                    device: Some(device.to_string()),
                    device_instance,
                    virtual_function_id: None,
                    ip_address: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                });

                if let Some(vf_network_segment_chunks) = vf_chunk_iter.next() {
                    for vf_network_segment_id in vf_network_segment_chunks {
                        interface_config.push(rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Virtual as i32,
                            network_segment_id: Some(*vf_network_segment_id), // to support legacy.
                            network_details: Some(NetworkDetails::SegmentId(
                                *vf_network_segment_id,
                            )),
                            device: Some(device.to_string()),
                            device_instance,
                            virtual_function_id: Some(vf_function_id),
                            ip_address: None,
                            ipv6_interface_config: None,
                            routing_profile: None,
                        });
                        vf_function_id += 1;
                    }
                }
            }

            (
                interface_config,
                allocate_instance
                    .tenant_org
                    .as_deref()
                    .unwrap_or("devenv_test_org")
                    .to_string(),
                None,
            )
        } else if !allocate_instance.vpc_prefix_id.is_empty() {
            let Some(discovery_info) = &machine.discovery_info else {
                return Err(CarbideCliError::GenericError(
                    "Machine discovery info is required for VPC prefix allocation.".to_string(),
                ));
            };
            // Create a vector of interface configs for each VPC prefix.  only Mellanox devices are supported.
            let mut interface_index_map = HashMap::new();
            let mut interface_configs = Vec::new();
            let pf_vpc_prefix_ids = &allocate_instance.vpc_prefix_id;
            let vf_vpc_prefix_ids = &allocate_instance.vf_vpc_prefix_id;

            let vfs_per_pf = if vf_vpc_prefix_ids.len() < pf_vpc_prefix_ids.len() {
                1
            } else {
                // pf_vpc_prefix_ids is checked for empty above (len() cannot be 0)
                vf_vpc_prefix_ids.len() / pf_vpc_prefix_ids.len()
            };
            tracing::debug!(vfs_per_pf, "VFs per PF",);
            let mut vf_chunk_iter = vf_vpc_prefix_ids.chunks(vfs_per_pf);
            let mut ipv6_vf_chunk_iter = allocate_instance.ipv6_vf_prefix_id.chunks(vfs_per_pf);
            for (map_index, i) in discovery_info
                .network_interfaces
                .iter()
                .filter(|i| {
                    i.pci_properties
                        .as_ref()
                        .is_some_and(|pci| pci.vendor.to_ascii_lowercase().contains("mellanox"))
                })
                .enumerate()
            {
                if let Some(pci_properties) = &i.pci_properties {
                    let Some(vpc_prefix_id) = allocate_instance.vpc_prefix_id.get(map_index) else {
                        tracing::debug!("No more vpc prefix ids; done");
                        break;
                    };

                    let device_instance = *interface_index_map
                        .entry(pci_properties.device.as_str())
                        .and_modify(|c| *c += 1)
                        .or_insert(0u32);

                    let new_interface = rpc::InstanceInterfaceConfig {
                        function_type: rpc::InterfaceFunctionType::Physical as i32,
                        network_segment_id: None,
                        network_details: Some(NetworkDetails::VpcPrefixId(*vpc_prefix_id)),
                        device: Some(pci_properties.device.clone()),
                        device_instance,
                        virtual_function_id: None,
                        ip_address: allocate_instance.ip_address.get(map_index).cloned(),
                        ipv6_interface_config: allocate_instance
                            .ipv6_vpc_prefix_id
                            .get(map_index)
                            .copied()
                            .map(|vpc_prefix_id| rpc::InstanceInterfaceIpv6Config {
                                vpc_prefix_id: Some(vpc_prefix_id),
                                ip_address: allocate_instance
                                    .ipv6_ip_address
                                    .get(map_index)
                                    .cloned(),
                            }),
                        routing_profile: None,
                    };
                    tracing::debug!(
                        new_interface = ?new_interface,
                        "Adding interface",
                    );

                    interface_configs.push(new_interface);

                    if let Some(vf_prefix_chunks) = vf_chunk_iter.next() {
                        let ipv6_vf_chunk = ipv6_vf_chunk_iter.next();
                        for (vf_idx, vf_vpc_prefix_id) in vf_prefix_chunks.iter().enumerate() {
                            let new_interface = rpc::InstanceInterfaceConfig {
                                function_type: rpc::InterfaceFunctionType::Virtual as i32,
                                network_segment_id: None,
                                network_details: Some(NetworkDetails::VpcPrefixId(
                                    *vf_vpc_prefix_id,
                                )),
                                device: Some(pci_properties.device.clone()),
                                device_instance,
                                virtual_function_id: Some(vf_function_id),
                                ip_address: allocate_instance.vf_ip_address.get(vf_idx).cloned(),
                                ipv6_interface_config: ipv6_vf_chunk
                                    .and_then(|c| c.get(vf_idx))
                                    .copied()
                                    .map(|vpc_prefix_id| rpc::InstanceInterfaceIpv6Config {
                                        vpc_prefix_id: Some(vpc_prefix_id),
                                        ip_address: ipv6_vf_chunk.and_then(|_| {
                                            allocate_instance
                                                .ipv6_vf_ip_address
                                                .get(vf_idx)
                                                .cloned()
                                        }),
                                    }),
                                routing_profile: None,
                            };
                            vf_function_id += 1;
                            tracing::debug!(
                                new_interface = ?new_interface,
                                "Adding interface",
                            );
                            interface_configs.push(new_interface);
                        }
                    }
                } else {
                    tracing::debug!(
                        interface = ?i,
                        "No PCI device info",
                    );
                }
            }

            (
                interface_configs,
                allocate_instance.tenant_org.clone().ok_or_else(|| {
                    CarbideCliError::GenericError(
                        "Tenant org is mandatory in case of vpc_prefix.".to_string(),
                    )
                })?,
                None,
            )
        } else {
            return Err(CarbideCliError::GenericError(
                "Either network segment id or vpc_prefix id is needed.".to_string(),
            ));
        };

        if allocate_instance.flat_vpc_id.is_none()
            && interface_configs.len()
                != (allocate_instance.subnet.len()
                    + allocate_instance.vf_subnet.len()
                    + allocate_instance.vpc_prefix_id.len()
                    + allocate_instance.vf_vpc_prefix_id.len())
        {
            return Err(CarbideCliError::GenericError(
                "Could not create the correct number of interface configs to satisfy request."
                    .to_string(),
            ));
        }
        let tenant_config = rpc::TenantConfig {
            tenant_organization_id: tenant_org,
            tenant_keyset_ids: vec![],
            hostname: None,
        };

        let instance_config = rpc::InstanceConfig {
            tenant: Some(tenant_config),
            os: allocate_instance.os.clone(),
            network: Some(rpc::InstanceNetworkConfig {
                interfaces: interface_configs,
                auto_config: vpc_id.map(|vpc_id| rpc::InstanceNetworkAutoConfig {
                    vpc_id: Some(vpc_id),
                }),
                #[allow(deprecated)]
                auto: allocate_instance.flat_vpc_id.is_some(),
            }),
            network_security_group_id: allocate_instance.network_security_group_id.clone(),
            infiniband: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: allocate_instance.spxconfig.clone(),
            power_profile: None,
        };

        let mut labels = vec![
            rpc::Label {
                key: String::from("cloud-unsafe-op"),
                value: None,
            },
            rpc::Label {
                key: String::from("admin-cli-last-modified-by"),
                value: modified_by,
            },
        ];

        match (&allocate_instance.label_key, &allocate_instance.label_value) {
            (None, Some(_)) => {
                tracing::error!("label key cannot be empty while value is not empty.");
            }
            (Some(key), value) => labels.push(rpc::Label {
                key: key.to_string(),
                value: value.clone(),
            }),
            (None, None) => {}
        }

        let instance_request = rpc::InstanceAllocationRequest {
            instance_id: None,
            machine_id: machine
                .id
                .map(carbide_uuid::machine::StableHostMachineId::try_from)
                .transpose()
                .map_err(|error| CarbideCliError::GenericError(error.to_string()))?,

            instance_type_id: allocate_instance.instance_type_id.clone(),
            config: Some(instance_config),
            metadata: Some(rpc::Metadata {
                name: instance_name.to_string(),
                description: "instance created from admin-cli".to_string(),
                labels,
            }),
            allow_unhealthy_machine: false,
        };

        tracing::trace!("{}", serde_json::to_string(&instance_request)?);
        Ok(instance_request)
    }

    pub(crate) async fn allocate_instance(
        &self,
        machine: Machine,
        allocate_instance: &AllocateInstance,
        instance_name: &str,
        modified_by: Option<String>,
    ) -> CarbideCliResult<rpc::Instance> {
        let request = self
            .build_instance_request(machine, allocate_instance, instance_name, modified_by)
            .await?;
        Ok(self.0.allocate_instance(request).await?)
    }

    /// Batch allocate instances (all-or-nothing).
    pub(crate) async fn allocate_instances(
        &self,
        requests: Vec<rpc::InstanceAllocationRequest>,
    ) -> CarbideCliResult<Vec<rpc::Instance>> {
        let response = self
            .0
            .allocate_instances(rpc::BatchInstanceAllocationRequest {
                instance_requests: requests,
            })
            .await?;
        Ok(response.instances)
    }

    /// Applies patches to a running instances configuration
    /// The function fetches the current configuration, and then calls the two
    /// `modify` closures to apply updates to the configuration.
    /// It then calls the `UpdateInstanceConfig` API to submit the updates
    /// to carbide.
    pub(crate) async fn update_instance_config_with(
        &self,
        instance_id: InstanceId,
        modify_config: impl FnOnce(&mut rpc::InstanceConfig),
        modify_metadata: impl FnOnce(&mut rpc::Metadata),
        modified_by: Option<String>,
    ) -> CarbideCliResult<rpc::Instance> {
        let find_response = self.0.find_instances_by_ids(vec![instance_id]).await?;

        let instance = find_response
            .instances
            .into_iter()
            .next()
            .ok_or_else(|| CarbideCliError::InstanceNotFound(instance_id))?;

        let config = instance.config.map(|mut c| {
            modify_config(&mut c);
            c
        });

        tracing::info!("{}", serde_json::to_string(&config).unwrap_or_default());

        let metadata = instance.metadata.map(|mut m| {
            modify_metadata(&mut m);

            let mut labels: Vec<rpc::Label> = m
                .labels
                .into_iter()
                .filter(|l| l.key != "cloud-unsafe-op" && l.key != "admin-cli-last-modified-by")
                .collect();
            labels.push(rpc::Label {
                key: String::from("cloud-unsafe-op"),
                value: None,
            });
            labels.push(rpc::Label {
                key: String::from("admin-cli-last-modified-by"),
                value: modified_by,
            });
            m.labels = labels;

            m
        });

        let update_instance_request = rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance_id),
            if_version_match: Some(instance.config_version),
            config,
            metadata,
        };
        Ok(self
            .0
            .update_instance_config(update_instance_request)
            .await?)
    }

    pub(crate) async fn set_container_registry_credential(
        &self,
        registry: String,
        username: String,
        password: String,
    ) -> CarbideCliResult<()> {
        Ok(self
            .0
            .set_container_registry_credential(rpc::SetContainerRegistryCredentialRequest {
                registry,
                username,
                password,
            })
            .await?)
    }

    pub(crate) async fn add_update_machine_validation_external_config(
        &self,
        name: String,
        description: String,
        config: Vec<u8>,
    ) -> CarbideCliResult<()> {
        let request = rpc::AddUpdateMachineValidationExternalConfigRequest {
            name,
            description: Some(description),
            config,
        };
        Ok(self
            .0
            .add_update_machine_validation_external_config(request)
            .await?)
    }

    pub(crate) async fn get_machine_validation_results(
        &self,
        machine_id: Option<MachineId>,
        history: bool,
        validation_id: Option<MachineValidationId>,
    ) -> CarbideCliResult<rpc::MachineValidationResultList> {
        let request = rpc::MachineValidationGetRequest {
            machine_id,
            include_history: history,
            validation_id,
        };
        Ok(self.0.get_machine_validation_results(request).await?)
    }

    pub(crate) async fn get_machine_validation_runs(
        &self,
        machine_id: Option<MachineId>,
        include_history: bool,
    ) -> CarbideCliResult<rpc::MachineValidationRunList> {
        let request = rpc::MachineValidationRunListGetRequest {
            machine_id,
            include_history,
        };
        Ok(self.0.get_machine_validation_runs(request).await?)
    }

    pub(crate) async fn on_demand_machine_validation(
        &self,
        machine_id: MachineId,
        tags: Option<Vec<String>>,
        allowed_tests: Option<Vec<String>>,
        run_unverified_tests: bool,
        contexts: Option<Vec<String>>,
    ) -> CarbideCliResult<rpc::MachineValidationOnDemandResponse> {
        let allowed_tests: Vec<String> = allowed_tests
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        let request = rpc::MachineValidationOnDemandRequest {
            machine_id: Some(machine_id),
            tags: tags.unwrap_or_default(),
            allowed_tests,
            action: rpc::machine_validation_on_demand_request::Action::Start.into(),
            run_unverfied_tests: run_unverified_tests,
            contexts: contexts.unwrap_or_default(),
        };
        Ok(self.0.on_demand_machine_validation(request).await?)
    }

    pub(crate) async fn on_demand_rack_maintenance(
        &self,
        rack_id: RackId,
        machine_ids: Vec<String>,
        switch_ids: Vec<String>,
        power_shelf_ids: Vec<String>,
        activities: Vec<rpc::MaintenanceActivityConfig>,
    ) -> CarbideCliResult<rpc::RackMaintenanceOnDemandResponse> {
        let request = rpc::RackMaintenanceOnDemandRequest {
            rack_id: Some(rack_id),
            scope: Some(rpc::RackMaintenanceScope {
                machine_ids,
                switch_ids,
                power_shelf_ids,
                activities,
            }),
        };
        Ok(self.0.on_demand_rack_maintenance(request).await?)
    }

    pub(crate) async fn terminate_rack_maintenance(
        &self,
        rack_id: RackId,
    ) -> CarbideCliResult<rpc::RackMaintenanceTerminateResponse> {
        let request = rpc::RackMaintenanceTerminateRequest {
            rack_id: Some(rack_id),
        };
        Ok(self.0.terminate_rack_maintenance(request).await?)
    }

    pub(crate) async fn list_os_image(
        &self,
        tenant_organization_id: Option<String>,
    ) -> CarbideCliResult<Vec<rpc::OsImage>> {
        let request = rpc::ListOsImageRequest {
            tenant_organization_id,
        };
        let response = self.0.list_os_image(request).await?;
        Ok(response.images)
    }

    pub(crate) async fn update_os_image(
        &self,
        id: ::rpc::common::Uuid,
        auth_type: Option<String>,
        auth_token: Option<String>,
        name: Option<String>,
        description: Option<String>,
    ) -> CarbideCliResult<rpc::OsImage> {
        let os_image = self.0.get_os_image(id).await?;
        let Some(mut new_attrs) = os_image.attributes else {
            return Err(CarbideCliError::Empty);
        };
        if auth_type.is_some() {
            new_attrs.auth_type = auth_type;
        }
        if auth_token.is_some() {
            new_attrs.auth_token = auth_token;
        }
        if name.is_some() {
            new_attrs.name = name;
        }
        if description.is_some() {
            new_attrs.description = description;
        }
        Ok(self.0.update_os_image(new_attrs).await?)
    }

    pub(crate) async fn update_instance_config(
        &self,
        instance_id: InstanceId,
        version: String,
        config: rpc::InstanceConfig,
        metadata: Option<rpc::Metadata>,
    ) -> CarbideCliResult<rpc::Instance> {
        let request = rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance_id),
            if_version_match: Some(version),
            config: Some(config),
            metadata,
        };
        Ok(self.0.update_instance_config(request).await?)
    }

    pub(crate) async fn update_vpc_config(
        &self,
        vpc_id: VpcId,
        version: String,
        metadata: Option<rpc::Metadata>,
        network_security_group_id: Option<String>,
    ) -> CarbideCliResult<rpc::Vpc> {
        let request = rpc::VpcUpdateRequest {
            id: Some(vpc_id),
            if_version_match: Some(version),
            metadata,
            network_security_group_id,
            default_nvlink_logical_partition_id: None,
            routing_profile_overrides: None,
            power_resource_group: None,
        };
        self.0
            .update_vpc(request)
            .await?
            .vpc
            .ok_or(CarbideCliError::Empty)
    }

    pub(crate) async fn get_machine_validation_tests(
        &self,
        test_id: Option<String>,
        platforms: Vec<String>,
        contexts: Vec<String>,
        show_un_verified: bool,
    ) -> CarbideCliResult<rpc::MachineValidationTestsGetResponse> {
        let verified = if show_un_verified { None } else { Some(true) };
        let request = rpc::MachineValidationTestsGetRequest {
            supported_platforms: platforms,
            contexts,
            test_id,
            verified,
            ..rpc::MachineValidationTestsGetRequest::default()
        };
        Ok(self.0.get_machine_validation_tests(request).await?)
    }

    pub(crate) async fn update_machine_metadata(
        &self,
        machine_id: MachineId,
        metadata: ::rpc::forge::Metadata,
        current_version: String,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::MachineMetadataUpdateRequest {
            machine_id: Some(machine_id),
            if_version_match: Some(current_version),
            metadata: Some(metadata),
        };
        Ok(self.0.update_machine_metadata(request).await?)
    }

    pub(crate) async fn update_rack_metadata(
        &self,
        rack_id: RackId,
        metadata: ::rpc::forge::Metadata,
        current_version: String,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::RackMetadataUpdateRequest {
            rack_id: Some(rack_id),
            if_version_match: Some(current_version),
            metadata: Some(metadata),
        };
        Ok(self.0.update_rack_metadata(request).await?)
    }

    pub(crate) async fn update_switch_metadata(
        &self,
        switch_id: SwitchId,
        metadata: ::rpc::forge::Metadata,
        current_version: String,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::SwitchMetadataUpdateRequest {
            switch_id: Some(switch_id),
            if_version_match: Some(current_version),
            metadata: Some(metadata),
        };
        Ok(self.0.update_switch_metadata(request).await?)
    }

    pub(crate) async fn update_power_shelf_metadata(
        &self,
        power_shelf_id: PowerShelfId,
        metadata: ::rpc::forge::Metadata,
        current_version: String,
    ) -> CarbideCliResult<()> {
        let request = ::rpc::forge::PowerShelfMetadataUpdateRequest {
            power_shelf_id: Some(power_shelf_id),
            if_version_match: Some(current_version),
            metadata: Some(metadata),
        };
        Ok(self.0.update_power_shelf_metadata(request).await?)
    }

    pub(crate) async fn get_single_network_security_group(
        &self,
        id: String,
    ) -> CarbideCliResult<rpc::NetworkSecurityGroup> {
        self.0
            .find_network_security_groups_by_ids(FindNetworkSecurityGroupsByIdsRequest {
                tenant_organization_id: None,
                network_security_group_ids: vec![id],
            })
            .await?
            .network_security_groups
            .pop()
            .ok_or(CarbideCliError::Empty)
    }

    pub(crate) async fn get_network_security_group_attachments(
        &self,
        id: String,
    ) -> CarbideCliResult<rpc::NetworkSecurityGroupAttachments> {
        self.0
            .get_network_security_group_attachments(GetNetworkSecurityGroupAttachmentsRequest {
                network_security_group_ids: vec![id],
            })
            .await?
            .attachments
            .pop()
            .ok_or(CarbideCliError::Empty)
    }

    pub(crate) async fn get_network_security_group_propagation_status(
        &self,
        id: String,
        vpc_ids: Option<Vec<String>>,
        instance_ids: Option<Vec<String>>,
    ) -> CarbideCliResult<(
        Vec<rpc::NetworkSecurityGroupPropagationObjectStatus>,
        Vec<rpc::NetworkSecurityGroupPropagationObjectStatus>,
    )> {
        let nsg = self
            .0
            .get_network_security_group_propagation_status(
                GetNetworkSecurityGroupPropagationStatusRequest {
                    network_security_group_ids: Some(rpc::NetworkSecurityGroupIdList {
                        ids: vec![id],
                    }),
                    vpc_ids: vpc_ids.unwrap_or_default(),
                    instance_ids: instance_ids.unwrap_or_default(),
                },
            )
            .await?;

        Ok((nsg.vpcs, nsg.instances))
    }

    pub(crate) async fn get_all_network_security_groups(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<Vec<rpc::NetworkSecurityGroup>> {
        let all_nsg_ids = self
            .0
            .find_network_security_group_ids(rpc::FindNetworkSecurityGroupIdsRequest {
                name: None,
                tenant_organization_id: None,
            })
            .await?
            .network_security_group_ids;

        let mut all_nsgs = Vec::with_capacity(all_nsg_ids.len());

        for nsg_ids in all_nsg_ids.chunks(self.effective_chunk_size(page_size).await?) {
            let nsgs = self
                .0
                .find_network_security_groups_by_ids(FindNetworkSecurityGroupsByIdsRequest {
                    tenant_organization_id: None,
                    network_security_group_ids: nsg_ids.to_vec(),
                })
                .await?
                .network_security_groups;
            all_nsgs.extend(nsgs);
        }

        Ok(all_nsgs)
    }

    pub(crate) async fn update_network_security_group(
        &self,
        id: String,
        tenant_organization_id: String,
        metadata: rpc::Metadata,
        if_version_match: Option<String>,
        stateful_egress: bool,
        rules: Vec<rpc::NetworkSecurityGroupRuleAttributes>,
    ) -> CarbideCliResult<rpc::NetworkSecurityGroup> {
        let request = UpdateNetworkSecurityGroupRequest {
            id,
            tenant_organization_id,
            metadata: Some(metadata),
            if_version_match,
            network_security_group_attributes: Some(NetworkSecurityGroupAttributes {
                stateful_egress,
                rules,
            }),
        };

        let response = self.0.update_network_security_group(request).await?;

        response
            .network_security_group
            .ok_or(CarbideCliError::Empty)
    }

    // TODO: add other hardware info
    pub(crate) async fn update_machine_hardware_info(
        &self,
        id: MachineId,
        hardware_info_update_type: MachineHardwareInfoUpdateType,
        gpus: Vec<::rpc::machine_discovery::Gpu>,
    ) -> CarbideCliResult<()> {
        let hardware_info = MachineHardwareInfo { gpus };
        Ok(self
            .0
            .update_machine_hardware_info(UpdateMachineHardwareInfoRequest {
                machine_id: Some(id),
                info: Some(hardware_info),
                update_type: hardware_info_update_type as i32,
            })
            .await?)
    }

    pub(crate) async fn get_all_instance_types(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<Vec<rpc::InstanceType>> {
        let all_ids = self.0.find_instance_type_ids().await?.instance_type_ids;

        let mut all_itypes = Vec::with_capacity(all_ids.len());

        for ids in all_ids.chunks(self.effective_chunk_size(page_size).await?) {
            let itypes = self
                .0
                .find_instance_types_by_ids(FindInstanceTypesByIdsRequest {
                    instance_type_ids: ids.to_vec(),
                    tenant_organization_id: None,
                    include_allocation_stats: false, // For showing all instance types in the CLI, we can skip allocation stats calculation.
                })
                .await?
                .instance_types;
            all_itypes.extend(itypes);
        }

        Ok(all_itypes)
    }

    pub(crate) async fn get_power_options(
        &self,
        machine_id: Vec<carbide_uuid::machine::HostMachineId>,
    ) -> CarbideCliResult<Vec<rpc::PowerOptions>> {
        let all_options = self
            .0
            .get_power_options(rpc::PowerOptionRequest { machine_id })
            .await?
            .response;

        Ok(all_options)
    }

    pub(crate) async fn get_all_nv_link_partitions(
        &self,
        tenant_org_id: Option<String>,
        name: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::NvLinkPartitionList> {
        let all_ids = self.get_nv_link_partition_ids(tenant_org_id, name).await?;
        let mut all_list = rpc::NvLinkPartitionList {
            partitions: Vec::with_capacity(all_ids.partition_ids.len()),
        };

        for ids in all_ids
            .partition_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_nv_link_partitions_by_ids(ids).await?;
            all_list.partitions.extend(list.partitions);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_nv_link_partition(
        &self,
        nvl_partition_id: NvLinkPartitionId,
    ) -> CarbideCliResult<rpc::NvLinkPartition> {
        let partitions = self
            .get_nv_link_partitions_by_ids(std::slice::from_ref(&nvl_partition_id))
            .await?;

        partitions.partitions.into_only_one_or_else(|_| {
            CarbideCliError::GenericError("Unknown NvLink Partition ID".to_string())
        })
    }

    async fn get_nv_link_partition_ids(
        &self,
        tenant_organization_id: Option<String>,
        name: Option<String>,
    ) -> CarbideCliResult<rpc::NvLinkPartitionIdList> {
        let request = rpc::NvLinkPartitionSearchFilter {
            tenant_organization_id,
            name,
        };
        self.0
            .find_nv_link_partition_ids(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    async fn get_nv_link_partitions_by_ids(
        &self,
        ids: &[NvLinkPartitionId],
    ) -> CarbideCliResult<rpc::NvLinkPartitionList> {
        let request = rpc::NvLinkPartitionsByIdsRequest {
            partition_ids: Vec::from(ids),
            include_history: ids.len() == 1,
        };
        self.0
            .find_nv_link_partitions_by_ids(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    pub(crate) async fn get_all_logical_partitions(
        &self,
        name: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::NvLinkLogicalPartitionList> {
        let all_ids = self.get_logical_partition_ids(name).await?;
        let mut all_list = rpc::NvLinkLogicalPartitionList {
            partitions: Vec::with_capacity(all_ids.partition_ids.len()),
        };

        for ids in all_ids
            .partition_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let list = self.get_logical_partitions_by_ids(ids).await?;
            all_list.partitions.extend(list.partitions);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_one_logical_partition(
        &self,
        partition_id: NvLinkLogicalPartitionId,
    ) -> CarbideCliResult<rpc::NvLinkLogicalPartition> {
        let partitions = self
            .get_logical_partitions_by_ids(std::slice::from_ref(&partition_id))
            .await?;

        partitions.partitions.into_only_one_or_else(|len| {
            CarbideCliError::GenericError(format!(
                "Expected a single logical partition found for ID: {partition_id}, found {len}",
            ))
        })
    }

    async fn get_logical_partition_ids(
        &self,
        name: Option<String>,
    ) -> CarbideCliResult<rpc::NvLinkLogicalPartitionIdList> {
        let request = rpc::NvLinkLogicalPartitionSearchFilter { name };
        self.0
            .find_nv_link_logical_partition_ids(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    async fn get_logical_partitions_by_ids(
        &self,
        ids: &[NvLinkLogicalPartitionId],
    ) -> CarbideCliResult<rpc::NvLinkLogicalPartitionList> {
        let request = rpc::NvLinkLogicalPartitionsByIdsRequest {
            partition_ids: Vec::from(ids),
            include_history: ids.len() == 1,
        };
        self.0
            .find_nv_link_logical_partitions_by_ids(request)
            .await
            .map_err(CarbideCliError::ApiInvocationError)
    }

    pub(crate) async fn enable_infinite_boot(
        &self,
        bmc_endpoint_request: Option<BmcEndpointRequest>,
        machine_id: Option<String>,
    ) -> CarbideCliResult<rpc::EnableInfiniteBootResponse> {
        let request = rpc::EnableInfiniteBootRequest {
            bmc_endpoint_request,
            machine_id,
        };
        Ok(self.0.enable_infinite_boot(request).await?)
    }

    pub(crate) async fn lockdown(
        &self,
        bmc_endpoint_request: Option<BmcEndpointRequest>,
        machine_id: MachineId,
        action: rpc::LockdownAction,
    ) -> CarbideCliResult<rpc::LockdownResponse> {
        let request = rpc::LockdownRequest {
            bmc_endpoint_request,
            machine_id: Some(machine_id),
            action: Some(action as i32),
        };
        Ok(self.0.lockdown(request).await?)
    }

    pub(crate) async fn get_remediation(
        &self,
        remediation_id: RemediationId,
    ) -> CarbideCliResult<Remediation> {
        let remediation_list = RemediationIdList {
            remediation_ids: vec![remediation_id],
        };

        let response = self.0.find_remediations_by_ids(remediation_list).await?;

        response
            .remediations
            .into_iter()
            .next()
            .ok_or(CarbideCliError::RemediationNotFound(remediation_id))
    }

    pub(crate) async fn get_all_remediations(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<RemediationList> {
        let all_remediation_ids = self.0.find_remediation_ids().await?;

        let remediations = stream::iter(
            all_remediation_ids
                .remediation_ids
                .chunks(self.effective_chunk_size(page_size).await?),
        )
        .then(|remediation_ids| async move {
            self.0
                .find_remediations_by_ids(remediation_ids)
                .await
                .map_err(CarbideCliError::ApiInvocationError)
        })
        .try_fold(vec![], |mut accum, remediations| async move {
            accum.extend(remediations.remediations);
            Ok(accum)
        })
        .await?;
        Ok(RemediationList { remediations })
    }

    pub(crate) async fn find_extension_services(
        &self,
        service_type: Option<i32>,
        name: Option<String>,
        tenant_organization_id: Option<String>,
        page_size: usize,
    ) -> CarbideCliResult<rpc::DpuExtensionServiceList> {
        let filter = rpc::DpuExtensionServiceSearchFilter {
            service_type,
            name,
            tenant_organization_id,
        };
        let ids_response = self.0.find_dpu_extension_service_ids(filter).await?;

        let mut all_list = rpc::DpuExtensionServiceList {
            services: Vec::with_capacity(ids_response.service_ids.len()),
        };

        for ids in ids_response
            .service_ids
            .chunks(self.effective_chunk_size(page_size).await?)
        {
            let request = rpc::DpuExtensionServicesByIdsRequest {
                service_ids: ids.to_vec(),
            };
            let list = self.0.find_dpu_extension_services_by_ids(request).await?;
            all_list.services.extend(list.services);
        }

        Ok(all_list)
    }

    pub(crate) async fn get_extension_service_by_id(
        &self,
        service_id: String,
    ) -> CarbideCliResult<rpc::DpuExtensionService> {
        let request = rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id],
        };

        let service_response = self.0.find_dpu_extension_services_by_ids(request).await?;

        service_response.services.into_only_one_or_else(|len| {
            if len == 0 {
                CarbideCliError::GenericError("Extension service not found".to_string())
            } else {
                CarbideCliError::GenericError(
                    "Multiple extension services found for the same ID".to_string(),
                )
            }
        })
    }

    pub(crate) async fn modify_dpf_state(
        &self,
        machine_id: HostMachineId,
        state: bool,
    ) -> CarbideCliResult<()> {
        let request = ModifyDpfStateRequest {
            machine_id: Some(machine_id),
            dpf_enabled: state,
        };

        Ok(self.0.modify_dpf_state(request).await?)
    }

    pub(crate) async fn get_dpf_state(
        &self,
        machine_ids: Vec<HostMachineId>,
        page_size: usize,
    ) -> CarbideCliResult<Vec<rpc::dpf_state_response::DpfState>> {
        let mut all_dpf_states = Vec::with_capacity(machine_ids.len());

        for machine_ids in machine_ids.chunks(self.effective_chunk_size(page_size).await?) {
            let request = GetDpfStateRequest {
                machine_ids: machine_ids.to_vec(),
            };
            let dpf_states = self.0.get_dpf_state(request).await?;
            all_dpf_states.extend(dpf_states.dpf_states);
        }

        Ok(all_dpf_states)
    }

    pub(crate) async fn get_dpf_host_snapshot(
        &self,
        host_machine_id: HostMachineId,
    ) -> CarbideCliResult<String> {
        let request = GetDpfHostSnapshotRequest {
            host_machine_id: Some(host_machine_id),
        };
        let response = self.0.get_dpf_host_snapshot(request).await?;
        Ok(response.json_payload)
    }

    /// Every machine DPF is waiting on, fetched a page at a time.
    ///
    /// The worklist is fleet-sized during a rollout, so ids come back in one
    /// call and their detail in chunks the server will accept.
    pub(crate) async fn list_pending_dpu_service_syncs(
        &self,
        page_size: usize,
    ) -> CarbideCliResult<Vec<PendingDpuServiceSync>> {
        let ids = self
            .0
            // Generated wrapper takes no argument: the request message is empty.
            .find_pending_dpu_service_sync_ids()
            .await?
            .machine_ids;

        let mut pending = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(self.effective_chunk_size(page_size).await?) {
            let page = self
                .0
                .find_pending_dpu_service_syncs_by_ids(FindPendingDpuServiceSyncsByIdsRequest {
                    machine_ids: chunk.to_vec(),
                })
                .await?;
            pending.extend(page.pending);
        }
        Ok(pending)
    }

    /// One machine's recorded history. Bounded by the server's retention, so it
    /// needs no paging.
    pub(crate) async fn list_dpu_service_sync_history(
        &self,
        machine_id: carbide_uuid::machine::HostMachineId,
    ) -> CarbideCliResult<Vec<PendingDpuServiceSync>> {
        let response = self
            .0
            .list_dpu_service_sync_history(ListDpuServiceSyncHistoryRequest {
                machine_id: Some(machine_id),
            })
            .await?;
        Ok(response.pending)
    }

    pub(crate) async fn release_dpu_service_sync_hold(
        &self,
        request: ReleaseDpuServiceSyncHoldRequest,
    ) -> CarbideCliResult<Vec<rpc::DpuServiceSyncReleaseResult>> {
        let response = self.0.release_dpu_service_sync_hold(request).await?;
        Ok(response.results)
    }

    pub(crate) async fn get_dpf_service_versions(
        &self,
    ) -> CarbideCliResult<Vec<rpc::DpfServiceVersion>> {
        let response = self.0.get_dpf_service_versions().await?;
        Ok(response.services)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::{
        LegacyBmcPatchFields, cap_chunk_size, legacy_bmc_patch_fields, maybe_unimplemented, rpc,
    };

    /// Inputs that differ across the `legacy_bmc_patch_fields` table.
    #[derive(Debug)]
    struct LegacyBmcPatchCase {
        bmc_ip_address_override: Option<String>,
        bmc_ip_allocation_override: Option<rpc::BmcIpAllocationType>,
        replacement_interfaces: Option<Vec<rpc::ExpectedInterface>>,
    }

    /// Builds the smallest protobuf interface needed by the patch-field table.
    fn expected_interface(role: rpc::ExpectedInterfaceRole) -> rpc::ExpectedInterface {
        rpc::ExpectedInterface {
            mac_address: "00:11:22:33:44:55".to_string(),
            role: Some(role as i32),
            ip_allocation: Some(rpc::ExpectedInterfaceIpAllocation::Dynamic as i32),
            ..Default::default()
        }
    }

    /// `PermissionDenied` must trigger the deprecated-alias fallback: servers
    /// that predate a renamed RPC reject its unknown method name with a bare
    /// HTTP 403 based on our internal RBAC rules, which tonic surfaces as
    /// `PermissionDenied` rather than `Unimplemented`.
    #[test]
    fn maybe_unimplemented_accepts_both_unknown_rpc_signals() {
        assert!(maybe_unimplemented(&tonic::Status::unimplemented("")));
        assert!(maybe_unimplemented(&tonic::Status::permission_denied("")));
    }

    #[test]
    fn maybe_unimplemented_rejects_other_errors() {
        for status in [
            tonic::Status::not_found(""),
            tonic::Status::unavailable(""),
            tonic::Status::internal(""),
            tonic::Status::invalid_argument(""),
            tonic::Status::unauthenticated(""),
            tonic::Status::failed_precondition(""),
        ] {
            assert!(
                !maybe_unimplemented(&status),
                "{:?} must not trigger the deprecated-alias fallback",
                status.code()
            );
        }
    }

    #[test]
    fn cap_chunk_size_respects_server_limit() {
        // A zero/unset cap means no server limit -- use page_size as-is.
        assert_eq!(cap_chunk_size(100, 0), 100);
        // A smaller cap wins, so a page never exceeds what the *ByIds RPCs accept.
        assert_eq!(cap_chunk_size(100, 40), 40);
        // A larger cap leaves page_size untouched.
        assert_eq!(cap_chunk_size(100, 500), 100);
        // Equal is a no-op.
        assert_eq!(cap_chunk_size(100, 100), 100);
    }

    /// Patch inputs retain omission, override, and explicit-clear semantics
    /// before the CLI sends its full-update request.
    #[test]
    fn legacy_bmc_patch_fields_preserve_presence_and_clear_semantics() {
        use rpc::{BmcIpAllocationType as LegacyAllocation, ExpectedInterfaceRole as Role};

        let existing = rpc::ExpectedMachine {
            bmc_mac_address: "00:11:22:33:44:55".to_string(),
            bmc_ip_address: Some("192.0.2.20".to_string()),
            bmc_ip_allocation: Some(LegacyAllocation::Fixed as i32),
            host_nics: vec![rpc::ExpectedInterface {
                fixed_ip: Some("192.0.2.20".to_string()),
                ip_allocation: None,
                ..expected_interface(Role::HostBmc)
            }],
            ..Default::default()
        };
        let expected_existing = LegacyBmcPatchFields {
            bmc_ip_address: existing.bmc_ip_address.clone(),
            bmc_ip_allocation: existing.bmc_ip_allocation,
        };

        check_values(
            [
                Check {
                    scenario: "an unrelated patch preserves both compatibility fields",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: None,
                        replacement_interfaces: None,
                    },
                    expect: expected_existing.clone(),
                },
                Check {
                    scenario: "a replacement without HostBmc preserves both compatibility fields",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: None,
                        replacement_interfaces: Some(vec![expected_interface(Role::Host)]),
                    },
                    expect: expected_existing,
                },
                Check {
                    scenario: "a HostBmc replacement omits both compatibility fields",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: None,
                        replacement_interfaces: Some(vec![expected_interface(Role::HostBmc)]),
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: None,
                        bmc_ip_allocation: None,
                    },
                },
                Check {
                    scenario: "an omitted HostBmc role can reset to inferred allocation",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: None,
                        replacement_interfaces: Some(vec![rpc::ExpectedInterface {
                            role: None,
                            fixed_ip: None,
                            ip_allocation: Some(
                                rpc::ExpectedInterfaceIpAllocation::Unspecified as i32,
                            ),
                            ..expected_interface(Role::HostBmc)
                        }]),
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: None,
                        bmc_ip_allocation: None,
                    },
                },
                Check {
                    scenario: "explicit compatibility fields override HostBmc",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: Some("192.0.2.40".to_string()),
                        bmc_ip_allocation_override: Some(LegacyAllocation::Fixed),
                        replacement_interfaces: Some(vec![expected_interface(Role::HostBmc)]),
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: Some("192.0.2.40".to_string()),
                        bmc_ip_allocation: Some(LegacyAllocation::Fixed as i32),
                    },
                },
                Check {
                    scenario: "an explicit dynamic policy clears the compatibility address",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: Some(LegacyAllocation::Dynamic),
                        replacement_interfaces: None,
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: Some(String::new()),
                        bmc_ip_allocation: Some(LegacyAllocation::Dynamic as i32),
                    },
                },
                Check {
                    scenario: "an explicit retained policy clears the compatibility address",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: None,
                        bmc_ip_allocation_override: Some(LegacyAllocation::Retained),
                        replacement_interfaces: None,
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: Some(String::new()),
                        bmc_ip_allocation: Some(LegacyAllocation::Retained as i32),
                    },
                },
                Check {
                    scenario: "an explicit address keeps the stored compatibility policy",
                    input: LegacyBmcPatchCase {
                        bmc_ip_address_override: Some("192.0.2.40".to_string()),
                        bmc_ip_allocation_override: None,
                        replacement_interfaces: None,
                    },
                    expect: LegacyBmcPatchFields {
                        bmc_ip_address: Some("192.0.2.40".to_string()),
                        bmc_ip_allocation: Some(LegacyAllocation::Fixed as i32),
                    },
                },
            ],
            |case| {
                legacy_bmc_patch_fields(
                    &existing,
                    case.bmc_ip_address_override,
                    case.bmc_ip_allocation_override,
                    case.replacement_interfaces.as_deref(),
                )
            },
        );
    }
}

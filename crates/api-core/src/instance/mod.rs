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

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use ::rpc::errors::RpcDataConversionError;
use ::rpc::forge as rpc;
use carbide_network::ip::IpAddressFamily;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::infiniband::IBPartitionId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::spx::SpxPartitionId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use config_version::ConfigVersion;
use db::vpc::VpcRowLock;
use db::{
    self, ObjectColumnFilter, ObjectFilter, compute_allocation, extension_service, ib_partition,
    network_security_group,
};
use ipnetwork::IpNetwork;
use itertools::Itertools;
use model::ConfigValidationError;
use model::dpa_interface::{DpaInterface, DpaSearchConfig};
use model::extension_service::{
    ExtensionService, ExtensionServiceLifecycleState, ExtensionServiceType,
};
use model::hardware_info::InfinibandInterface;
use model::ib::{DEFAULT_IB_FABRIC_NAME, IbMembership};
use model::ib_partition::PartitionKey;
use model::instance::NewInstance;
use model::instance::config::InstanceConfig;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::{
    InstanceInterfaceIpFamilyMode, InstanceNetworkConfig, InterfaceFunctionId, Ipv6InterfaceConfig,
    NetworkDetails,
};
use model::instance::config::spx::{InstanceSpxConfig, SpxAttachmentType};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    HostHealthConfig, LoadSnapshotOptions, Machine, ManagedHostStateSnapshot, NotAllocatableReason,
};
use model::metadata::Metadata;
use model::network_segment::NetworkSegmentType;
use model::os::OperatingSystemVariant;
use model::tenant::TenantOrganizationId;
use model::vpc::{
    FabricInterfaceType, VpcVirtualizationTypeCapabilities, instance_prefix_len,
    vpc_prefix_can_allocate_interface_prefix,
};
use model::vpc_prefix::VpcPrefix;
use sqlx::PgConnection;

use crate::api::Api;
use crate::cfg::file::{CarbideConfig, ComputeAllocationEnforcement};
use crate::ethernet_virtualization::validate_instance_interface_routing_profiles;
use crate::network_segment::allocate::PrefixAllocator;

/// Validate a requested IP address for a linknet allocation and wrap it as
/// an IpNetwork with the given prefix length. Returns an error if the host
/// bit is 0 (the DPU end of the linknet -- the host must use the ::1 end).
fn build_requested_linknet_prefix(
    ip: std::net::IpAddr,
    linknet_prefix_len: u8,
) -> CarbideResult<IpNetwork> {
    let host_bit_is_zero = match ip {
        std::net::IpAddr::V4(v4) => v4.to_bits() & 1 == 0,
        std::net::IpAddr::V6(v6) => v6.to_bits() & 1 == 0,
    };
    if host_bit_is_zero {
        return Err(CarbideError::InvalidConfiguration(
            ConfigValidationError::InvalidValue(format!(
                "requested IP address must not have final host bit of 0: {ip}",
            )),
        ));
    }
    IpNetwork::new(ip.to_canonical(), linknet_prefix_len).map_err(|e| CarbideError::Internal {
        message: format!("unable to create IP network for {ip}: {e}"),
    })
}
use crate::{CarbideError, CarbideResult};

async fn validate_zero_dpu_auto_vpc(
    txn: &mut PgConnection,
    vpc_id: VpcId,
    tenant_organization_id: &TenantOrganizationId,
) -> Result<model::vpc::Vpc, CarbideError> {
    let vpc = db::vpc::find_by_with_lock(
        txn,
        ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
        VpcRowLock::Mutation,
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| CarbideError::FailedPrecondition(format!("VPC `{vpc_id}` does not exist")))?;

    if vpc.config.tenant_organization_id != tenant_organization_id.to_string() {
        return Err(CarbideError::FailedPrecondition(format!(
            "VPC `{}` is not owned by tenant `{}`",
            vpc.id, tenant_organization_id
        )));
    }

    if vpc.config.network_virtualization_type != VpcVirtualizationType::Flat {
        return Err(CarbideError::FailedPrecondition(format!(
            "zero-DPU auto allocation requires a flat VPC; VPC {} uses {}",
            vpc.id, vpc.config.network_virtualization_type
        )));
    }

    let vpc_iface = vpc
        .config
        .network_virtualization_type
        .fabric_interface_type();
    if vpc_iface != FabricInterfaceType::Nic {
        return Err(CarbideError::FailedPrecondition(format!(
            "zero-DPU auto allocation requires a VPC whose fabric_interface_type is `nic`; VPC {} ({}) has `{vpc_iface}`",
            vpc.id, vpc.config.network_virtualization_type
        )));
    }

    Ok(vpc)
}

/// Rejects instance VFs that DPF did not materialize from the configured replacement topology.
///
/// DPF topology replaces the static PF/VF ServiceInterface inventory, making exact membership
/// authoritative. Pre-DPF bridging is only a sparse provisioning override, not an inventory;
/// omitting a representor does not remove it from the provisioned hardware population. The pre-DPF
/// and static DPF paths therefore retain their historical admission behavior. DPF startup owns
/// normalization of the raw topology, so this request-time gate only projects its already-validated
/// VF identities from immutable runtime configuration.
pub(crate) fn validate_instance_vfs_against_dpf_topology(
    network: &InstanceNetworkConfig,
    config: &CarbideConfig,
) -> CarbideResult<()> {
    validate_vf_ids_against_dpf_topology(
        network
            .interfaces
            .iter()
            .filter_map(|interface| match &interface.function_id {
                InterfaceFunctionId::Physical {} => None,
                InterfaceFunctionId::Virtual { id } => Some(*id),
            }),
        config,
    )
}

/// Applies exact topology membership to an already structurally validated VF sequence.
fn validate_vf_ids_against_dpf_topology(
    vf_ids: impl IntoIterator<Item = u8>,
    config: &CarbideConfig,
) -> CarbideResult<()> {
    let Some(selected_vfs) = dpf_topology_vf_ids(config) else {
        return Ok(());
    };

    if let Some(unselected_vf) = vf_ids
        .into_iter()
        .find(|vf_id| !selected_vfs.contains(vf_id))
    {
        return Err(ConfigValidationError::InvalidValue(format!(
            "virtual function VF{unselected_vf} is not selected by the configured DPF intercept-bridging topology"
        ))
        .into());
    }

    Ok(())
}

/// Returns the exact topology VF population, or `None` when legacy admission remains in effect.
fn dpf_topology_vf_ids(config: &CarbideConfig) -> Option<BTreeSet<u8>> {
    if !config.dpf.enabled {
        return None;
    }

    let topology = config.vmaas_config.as_ref()?.bridging.as_ref()?;
    Some(
        topology
            .host_representor_intercept_bridging
            .values()
            .filter_map(|interface| interface.dpf_interface?.vf_id)
            .collect(),
    )
}

/// Validates that an operating system definition referenced by ID exists, is active,
/// and has status READY.  Returns `Ok(())` when the OS variant is not
/// `OperatingSystemId` (inline iPXE / OS image variants need no lookup).
pub(crate) async fn validate_os_definition_usable(
    txn: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    os: &model::os::OperatingSystem,
) -> Result<(), CarbideError> {
    let os_id = match os.variant {
        OperatingSystemVariant::OperatingSystemId(id) => id,
        _ => return Ok(()),
    };
    let row = db::operating_system::get(txn, os_id).await.map_err(|e| {
        if e.is_not_found() {
            CarbideError::FailedPrecondition(format!("operating system `{os_id}` does not exist"))
        } else {
            CarbideError::internal(format!("failed to get operating system: {e}"))
        }
    })?;
    if !row.is_active {
        return Err(CarbideError::FailedPrecondition(format!(
            "operating system `{os_id}` is not active"
        )));
    }
    if row.status != db::operating_system::OS_STATUS_READY {
        return Err(CarbideError::FailedPrecondition(format!(
            "operating system `{os_id}` is not ready (status: {})",
            row.status
        )));
    }
    Ok(())
}

/// User parameters for creating an instance
#[derive(Debug)]
pub(crate) struct InstanceAllocationRequest {
    /// The Machine on top of which we create an Instance
    pub(crate) machine_id: MachineId,

    /// The expected InstanceTypeId of the source
    /// machine for the instance.
    pub(crate) instance_type_id: Option<InstanceTypeId>,

    /// Desired ID for the new instance
    pub(crate) instance_id: InstanceId,

    /// Desired configuration of the instance
    pub(crate) config: InstanceConfig,

    pub(crate) metadata: Metadata,

    /// Allow allocation on unhealthy machines
    pub(crate) allow_unhealthy_machine: bool,
}

fn normalize_created_power_profile(power_profile: Option<String>) -> Option<String> {
    power_profile.filter(|profile| !profile.is_empty())
}

impl TryFrom<rpc::InstanceAllocationRequest> for InstanceAllocationRequest {
    type Error = CarbideError;

    fn try_from(request: rpc::InstanceAllocationRequest) -> Result<Self, Self::Error> {
        let machine_id = request
            .machine_id
            .ok_or(RpcDataConversionError::MissingArgument("machine_id"))?;

        let instance_type_id = request
            .instance_type_id
            .map(|i| i.parse::<InstanceTypeId>())
            .transpose()
            .map_err(|e| {
                CarbideError::from(RpcDataConversionError::InvalidInstanceTypeId(e.value()))
            })?;

        let config = request
            .config
            .ok_or(RpcDataConversionError::MissingArgument("config"))?;

        let mut config = InstanceConfig::try_from(config)?;
        // Empty power-policy values are clear sentinels only on update. During
        // creation there is no association to clear, so persist them as unset.
        config.power_profile = normalize_created_power_profile(config.power_profile);

        // If the Tenant provides an instance ID use this one
        // Otherwise create a random ID
        let instance_id = request
            .instance_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().into());

        let metadata = match request.metadata {
            Some(metadata) => metadata.try_into()?,
            None => Metadata::new_with_default_name(),
        };

        let allow_unhealthy_machine = request.allow_unhealthy_machine;

        Ok(InstanceAllocationRequest {
            instance_id,
            instance_type_id,
            machine_id,
            config,
            metadata,
            allow_unhealthy_machine,
        })
    }
}

/// The initial candidate attempt plus one retry after an overlap conflict.
const PREFIX_ALLOCATION_TOTAL_ATTEMPTS: usize = 2;
const NETWORK_PREFIX_OVERLAP_CONSTRAINT: &str = "network_prefixes_prefix_excl";

/// Address-family component of a canonical allocation group.
///
/// Declaration order keeps IPv4 before IPv6 within each VPC, satisfying the
/// dual-stack create-before-attach dependency and cross-transaction group order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AllocationAddressFamily {
    Ipv4,
    Ipv6,
}

impl AllocationAddressFamily {
    /// Derives the family key from a network prefix.
    fn from_network(prefix: IpNetwork) -> Self {
        if prefix.is_ipv4() {
            Self::Ipv4
        } else {
            Self::Ipv6
        }
    }

    /// Derives the family key from an individual address.
    fn from_address(address: std::net::IpAddr) -> Self {
        if address.is_ipv4() {
            Self::Ipv4
        } else {
            Self::Ipv6
        }
    }

    /// Returns the representation used by the shared network model.
    fn ip_address_family(self) -> IpAddressFamily {
        match self {
            Self::Ipv4 => IpAddressFamily::Ipv4,
            Self::Ipv6 => IpAddressFamily::Ipv6,
        }
    }

    /// Returns the family name used in operator-facing errors.
    fn name(self) -> &'static str {
        match self {
            Self::Ipv4 => "IPv4",
            Self::Ipv6 => "IPv6",
        }
    }
}

/// Validates that an explicitly selected parent can contain one interface prefix.
fn validate_vpc_prefix_allocation_capacity(
    vpc_prefix: &VpcPrefix,
    family: AllocationAddressFamily,
    allocation_prefix_len: u8,
) -> CarbideResult<()> {
    if vpc_prefix_can_allocate_interface_prefix(
        family.ip_address_family(),
        vpc_prefix.config.prefix.prefix(),
        allocation_prefix_len,
    ) {
        return Ok(());
    }

    Err(CarbideError::InvalidArgument(format!(
        "VPC prefix `{}` ({}) cannot contain a /{} interface prefix",
        vpc_prefix.id, vpc_prefix.config.prefix, allocation_prefix_len,
    )))
}

/// Selects the database operation and interface fields for a family allocation.
///
/// `Primary` creates a segment and may use either family. Dual stack plans IPv4
/// as `Primary` and attaches IPv6 through `SecondaryIpv6`.
#[derive(Clone, Copy, Debug)]
enum PrefixAllocationSlot {
    Primary,
    SecondaryIpv6,
}

/// Database mutation selected from the allocation slot.
#[derive(Clone, Copy, Debug)]
enum PrefixAllocationOperation {
    /// Creates a generated segment for the primary family and returns its ID.
    Create,
    /// Attaches the selected IPv6 linknet to an existing dual-stack segment.
    Attach(NetworkSegmentId),
}

/// One unresolved interface-family allocation and its frozen candidate sequence.
#[derive(Clone, Debug)]
struct PrefixAllocationWork {
    // Stable coordinates into `targets`, used for dependency lookup and resolution.
    target_index: usize,
    interface_index: usize,

    // Canonical execution-group and candidate-revalidation identity.
    vpc_id: VpcId,
    family: AllocationAddressFamily,
    // Frozen from the owning VPC's immutable SLAAC policy during planning.
    allocation_prefix_len: u8,

    // Frozen ascending candidate IDs and their monotonically advancing cursor.
    // Candidate row state is re-read under lock during execution.
    candidates: Arc<[VpcPrefixId]>,
    candidate_index: usize,

    // Selects create-versus-attach; explicit work may pin the selected interface prefix.
    slot: PrefixAllocationSlot,
    requested_prefix: Option<IpNetwork>,

    // Only automatic intent may advance past a candidate deleted after planning.
    automatic: bool,
}

/// Per-request mutable network config and tenant organization used for VPC
/// ownership checks.
struct NetworkAllocationTarget<'a> {
    network_config: &'a mut InstanceNetworkConfig,
    tenant_organization_id: &'a TenantOrganizationId,
}

/// Owned discovery results used by synchronous allocation planning.
///
/// Candidate IDs are frozen before planning. Planning uses the VPCs and
/// explicitly selected prefixes found during discovery; execution reads each
/// selected prefix again under lock before allocating from it.
struct PrefixAllocationContext {
    // Explicit selections verified to exist and be active at discovery time.
    explicit_prefixes: HashMap<VpcPrefixId, VpcPrefix>,
    // Active-at-discovery candidates frozen by VPC/family in ascending ID order.
    automatic_candidates: BTreeMap<(VpcId, AllocationAddressFamily), Arc<[VpcPrefixId]>>,
    // Referenced VPCs verified to exist for ownership and capability checks.
    vpcs: HashMap<VpcId, model::vpc::Vpc>,
}

/// Result of allocating from one candidate.
#[derive(Clone, Copy, Debug)]
enum CandidateAllocationOutcome {
    Allocated {
        /// Present for segment creation and absent for attachment.
        network_segment_id: Option<NetworkSegmentId>,
    },
    /// The candidate has no unoccupied linknet for this family.
    Exhausted,
    /// The locking re-read found that the candidate is no longer active.
    Deleted,
}

/// Candidate-wide result cached while draining work queued on the same prefix.
#[derive(Clone, Copy, Debug)]
enum CandidateUnavailable {
    /// Candidate capacity was conclusively exhausted.
    Exhausted,
    /// The candidate became inactive after discovery.
    Deleted,
}

/// Expands a caller's family mode in canonical IPv4-before-IPv6 order.
fn requested_families(mode: &InstanceInterfaceIpFamilyMode) -> &'static [AllocationAddressFamily] {
    match mode {
        InstanceInterfaceIpFamilyMode::Ipv4Only => &[AllocationAddressFamily::Ipv4],
        InstanceInterfaceIpFamilyMode::Ipv6Only => &[AllocationAddressFamily::Ipv6],
        InstanceInterfaceIpFamilyMode::DualStack => {
            &[AllocationAddressFamily::Ipv4, AllocationAddressFamily::Ipv6]
        }
    }
}

/// Returns whether an interface still needs resources generated from a network
/// prefix.
fn interface_needs_prefix_allocation(
    interface: &model::instance::config::network::InstanceInterfaceConfig,
) -> bool {
    // Preserve the existing update contract: durable allocation results identify
    // a reused interface, while an interface whose prefix is unresolved must
    // be allocated even if a caller supplied a stale network_segment_id value.
    // SLAAC intentionally has no IPv6 address, so its retained interface prefix
    // is the durable resolution marker.
    interface.ip_addrs.is_empty() && interface.interface_prefixes.is_empty()
}

/// Checks the owning VPC's declared support for an address family.
fn vpc_supports_family(
    virtualization_type: VpcVirtualizationType,
    family: AllocationAddressFamily,
) -> bool {
    match family {
        AllocationAddressFamily::Ipv4 => virtualization_type.supports_ipv4_prefix(),
        AllocationAddressFamily::Ipv6 => virtualization_type.supports_ipv6_prefix(),
    }
}

/// Identifies the network-prefix overlap exclusion conflict that is safe to retry.
fn is_network_prefix_overlap_conflict(error: &CarbideError) -> bool {
    matches!(
        error,
        CarbideError::DBError(db::AnnotatedSqlxError {
            source: sqlx::Error::Database(database_error),
            ..
        }) if database_error.constraint() == Some(NETWORK_PREFIX_OVERLAP_CONSTRAINT)
    )
}

/// Performs one candidate allocation after the caller has established a savepoint.
async fn allocate_prefix_candidate_once(
    txn: &mut PgConnection,
    vpc_id: VpcId,
    family: AllocationAddressFamily,
    allocation_prefix_len: u8,
    vpc_prefix_id: VpcPrefixId,
    operation: PrefixAllocationOperation,
    requested_prefix: Option<IpNetwork>,
) -> CarbideResult<CandidateAllocationOutcome> {
    // Re-read under the candidate lock so deletion and cursor changes after
    // discovery are observed before allocation.
    let Some(vpc_prefix) = db::vpc_prefix::lock_for_allocation(txn, vpc_prefix_id).await? else {
        return Ok(CandidateAllocationOutcome::Deleted);
    };

    if vpc_prefix.vpc_id != vpc_id
        || AllocationAddressFamily::from_network(vpc_prefix.config.prefix) != family
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "VPC prefix `{vpc_prefix_id}` no longer belongs to the requested {} allocation group in VPC `{vpc_id}`",
            family.name(),
        )));
    }

    // Use a caller-pinned linknet when present; otherwise select the next free one.
    let allocator = PrefixAllocator::new(
        vpc_prefix.id,
        vpc_prefix.config.prefix,
        vpc_prefix.status.last_used_prefix,
        allocation_prefix_len,
    )?;
    let allocated_prefix = if let Some(requested_prefix) = requested_prefix {
        allocator
            .validate_desired_prefix(&mut *txn, requested_prefix)
            .await?;
        requested_prefix
    } else {
        match allocator.next_free_prefix(&mut *txn).await {
            Ok(prefix) => prefix,
            Err(CarbideError::ResourceExhausted(_)) => {
                return Ok(CandidateAllocationOutcome::Exhausted);
            }
            Err(error) => return Err(error),
        }
    };
    // Execute the operation selected from the preplanned slot. For dual stack,
    // planning assigns IPv4 `Create` and IPv6 `Attach`.
    let (network_segment_id, allocated_prefix) = match operation {
        PrefixAllocationOperation::Create => {
            let (network_segment_id, prefix) = allocator
                .allocate_network_segment_for_prefix(&mut *txn, vpc_id, allocated_prefix)
                .await?;
            (Some(network_segment_id), prefix)
        }
        PrefixAllocationOperation::Attach(network_segment_id) => {
            let prefix = allocator
                .allocate_linknet_for_segment_with_prefix(
                    &mut *txn,
                    network_segment_id,
                    allocated_prefix,
                )
                .await?;
            (None, prefix)
        }
    };

    // Persist the selected linknet as the next-fit cursor atomically with the
    // generated resource.
    db::vpc_prefix::update_last_used_prefix(&mut *txn, &vpc_prefix.id, allocated_prefix).await?;

    Ok(CandidateAllocationOutcome::Allocated { network_segment_id })
}

/// Attempts one candidate under a savepoint and retries one overlap race.
///
/// Every unsuccessful attempt explicitly rolls back its savepoint before the
/// caller advances or retries, releasing any lock acquired only by that attempt.
async fn attempt_prefix_candidate(
    txn: &mut PgConnection,
    vpc_id: VpcId,
    family: AllocationAddressFamily,
    allocation_prefix_len: u8,
    vpc_prefix_id: VpcPrefixId,
    operation: PrefixAllocationOperation,
    requested_prefix: Option<IpNetwork>,
) -> CarbideResult<CandidateAllocationOutcome> {
    for _ in 0..PREFIX_ALLOCATION_TOTAL_ATTEMPTS {
        let mut savepoint = db::Transaction::begin_inner(txn).await?;
        let allocation_result = allocate_prefix_candidate_once(
            savepoint.as_pgconn(),
            vpc_id,
            family,
            allocation_prefix_len,
            vpc_prefix_id,
            operation,
            requested_prefix,
        )
        .await;

        match allocation_result {
            Ok(outcome @ CandidateAllocationOutcome::Allocated { .. }) => {
                // Release the savepoint so its writes and locks remain in the
                // outer transaction.
                savepoint.commit().await?;
                return Ok(outcome);
            }
            Ok(CandidateAllocationOutcome::Deleted) => {
                savepoint.rollback().await?;
                return Ok(CandidateAllocationOutcome::Deleted);
            }
            Ok(CandidateAllocationOutcome::Exhausted) => {
                savepoint.rollback().await?;
                return Ok(CandidateAllocationOutcome::Exhausted);
            }
            Err(error) => {
                let overlap_conflict = is_network_prefix_overlap_conflict(&error);
                savepoint.rollback().await?;

                if overlap_conflict {
                    // Defensive path: ordinary allocations from this VPC prefix
                    // serialize on its row lock. A rare writer outside that
                    // protocol may insert an overlapping CIDR after our read;
                    // only that exclusion race is retryable.
                    continue;
                }
                return Err(error);
            }
        }
    }

    // A second overlap race leaves capacity unproven; let the caller retry
    // rather than carrying speculative state through the batch scheduler.
    Err(CarbideError::UnavailableError(format!(
        "allocation from {} VPC prefix `{vpc_prefix_id}` repeatedly raced with \
         an overlapping network-prefix write; retry the request",
        family.name(),
    )))
}

/// Advances one work item monotonically or returns its final classified error.
fn advance_prefix_work(
    work_index: usize,
    current_candidate: VpcPrefixId,
    unavailable: CandidateUnavailable,
    works: &mut [PrefixAllocationWork],
    pending: &mut BTreeMap<VpcPrefixId, VecDeque<usize>>,
) -> CarbideResult<()> {
    let work = works.get_mut(work_index).ok_or_else(|| {
        CarbideError::internal(format!(
            "prefix allocation work index {work_index} is out of bounds",
        ))
    })?;

    // Explicit intent has no fallback candidate; only automatic intent may
    // advance after deletion.
    if matches!(unavailable, CandidateUnavailable::Deleted) && !work.automatic {
        return Err(CarbideError::InvalidArgument(format!(
            "VPC prefix `{current_candidate}` is marked for deletion and cannot be used for allocation",
        )));
    }

    work.candidate_index += 1;
    let Some(next_candidate) = work.candidates.get(work.candidate_index).copied() else {
        let message = format!(
            "no eligible {} VPC prefix in VPC `{}` could allocate an interface linknet",
            work.family.name(),
            work.vpc_id,
        );
        return Err(CarbideError::ResourceExhausted(message));
    };

    // Candidate IDs must increase strictly to preserve canonical row-lock progression.
    if next_candidate <= current_candidate {
        return Err(CarbideError::internal(format!(
            "VPC prefix candidate order moved backward from `{current_candidate}` to `{next_candidate}`",
        )));
    }
    pending
        .entry(next_candidate)
        .or_default()
        .push_back(work_index);
    Ok(())
}

/// Persists a selected candidate into the interface's rolling-compatible fields.
fn apply_prefix_resolution(
    targets: &mut [NetworkAllocationTarget<'_>],
    work: &PrefixAllocationWork,
    vpc_prefix_id: VpcPrefixId,
    network_segment_id: Option<NetworkSegmentId>,
) -> CarbideResult<()> {
    let target = targets.get_mut(work.target_index).ok_or_else(|| {
        CarbideError::internal(format!(
            "network allocation target index {} is out of bounds",
            work.target_index,
        ))
    })?;
    let interface = target
        .network_config
        .interfaces
        .get_mut(work.interface_index)
        .ok_or_else(|| {
            CarbideError::internal(format!(
                "network interface index {} is out of bounds for allocation target {}",
                work.interface_index, work.target_index,
            ))
        })?;

    match work.slot {
        PrefixAllocationSlot::Primary => {
            let network_segment_id = network_segment_id.ok_or_else(|| {
                CarbideError::internal(
                    "primary VPC-prefix allocation did not create a network segment".to_string(),
                )
            })?;
            interface.network_segment_id = Some(network_segment_id);
            interface.network_details = Some(NetworkDetails::VpcPrefixId(vpc_prefix_id));
            interface.vpc_id = Some(work.vpc_id);
        }
        PrefixAllocationSlot::SecondaryIpv6 => {
            // Primary already recorded the shared VPC and segment, and allocation
            // already attached this linknet. Record its prefix while retaining
            // any explicit requested-address intent.
            let requested_ip_addr = interface
                .ipv6_interface_config
                .as_ref()
                .and_then(|config| config.requested_ip_addr);
            interface.ipv6_interface_config = Some(Ipv6InterfaceConfig {
                vpc_prefix_id,
                requested_ip_addr,
            });
        }
    }
    Ok(())
}

/// Loads the database-backed inputs needed to plan prefix allocations.
async fn load_prefix_allocation_context(
    targets: &[NetworkAllocationTarget<'_>],
    txn: &mut PgConnection,
) -> CarbideResult<Option<PrefixAllocationContext>> {
    // Discover explicit prefix IDs and automatic VPC intent before issuing reads.
    let mut explicit_prefix_ids = BTreeSet::new();
    let mut automatic_vpc_ids = BTreeSet::new();
    let mut automatic_candidate_vpc_ids = BTreeSet::new();

    for target in targets.iter() {
        for interface in &target.network_config.interfaces {
            if let Some(selection) = &interface.vpc_selection {
                automatic_vpc_ids.insert(selection.vpc_id);
                if interface_needs_prefix_allocation(interface) {
                    automatic_candidate_vpc_ids.insert(selection.vpc_id);
                }
                continue;
            }

            if let Some(NetworkDetails::VpcPrefixId(vpc_prefix_id)) =
                interface.network_details.as_ref()
            {
                explicit_prefix_ids.insert(*vpc_prefix_id);
            }
            if let Some(ipv6) = &interface.ipv6_interface_config {
                explicit_prefix_ids.insert(ipv6.vpc_prefix_id);
            }
        }
    }

    // Segment-backed and empty configs have no VPC-prefix intent to validate
    // or allocate.
    if explicit_prefix_ids.is_empty() && automatic_vpc_ids.is_empty() {
        return Ok(None);
    }

    // Validate explicit selections without locks; execution later acquires
    // candidate locks in canonical order.
    let explicit_prefix_ids = explicit_prefix_ids.into_iter().collect_vec();
    let explicit_prefixes = if explicit_prefix_ids.is_empty() {
        Vec::new()
    } else {
        db::vpc_prefix::get_for_allocation_by_ids(txn, &explicit_prefix_ids).await?
    };
    let explicit_prefixes: HashMap<VpcPrefixId, VpcPrefix> = explicit_prefixes
        .into_iter()
        .map(|prefix| (prefix.id, prefix))
        .collect();

    for vpc_prefix_id in &explicit_prefix_ids {
        let Some(vpc_prefix) = explicit_prefixes.get(vpc_prefix_id) else {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC prefix `{vpc_prefix_id}` does not exist",
            )));
        };
        if vpc_prefix.is_marked_as_deleted() {
            return Err(CarbideError::InvalidArgument(format!(
                "VPC prefix `{vpc_prefix_id}` is marked for deletion and cannot be used for allocation",
            )));
        }
    }

    // Load active automatic candidates. Their eligibility depends on the
    // owning VPC's SLAAC policy, which is loaded below.
    let automatic_candidate_vpc_ids = automatic_candidate_vpc_ids.into_iter().collect_vec();
    let automatic_prefixes = if automatic_candidate_vpc_ids.is_empty() {
        Vec::new()
    } else {
        db::vpc_prefix::find_allocation_candidates(txn, &automatic_candidate_vpc_ids).await?
    };

    // Load every referenced VPC once for ownership and capability validation.
    let mut referenced_vpc_ids = automatic_vpc_ids.clone();
    referenced_vpc_ids.extend(explicit_prefixes.values().map(|prefix| prefix.vpc_id));
    let referenced_vpc_ids = referenced_vpc_ids.into_iter().collect_vec();
    let vpcs = db::vpc::find_by(
        &mut *txn,
        ObjectColumnFilter::List(db::vpc::IdColumn, &referenced_vpc_ids),
    )
    .await?;
    let vpcs: HashMap<VpcId, model::vpc::Vpc> = vpcs.into_iter().map(|vpc| (vpc.id, vpc)).collect();

    // Distinguish caller-supplied automatic intent from persisted prefix-parent state.
    for vpc_id in &automatic_vpc_ids {
        if !vpcs.contains_key(vpc_id) {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC `{vpc_id}` selected for automatic VPC-prefix allocation \
                 does not exist or is deleted",
            )));
        }
    }
    for prefix in explicit_prefixes.values() {
        if !vpcs.contains_key(&prefix.vpc_id) {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC prefix `{}` references VPC `{}`, which does not exist or is deleted",
                prefix.id, prefix.vpc_id,
            )));
        }
    }

    // Freeze only candidates that can contain an instance allocation under
    // the owning VPC's policy.
    let mut automatic_candidates: BTreeMap<(VpcId, AllocationAddressFamily), Vec<VpcPrefixId>> =
        BTreeMap::new();
    for prefix in automatic_prefixes {
        let Some(vpc) = vpcs.get(&prefix.vpc_id) else {
            continue;
        };
        let family = AllocationAddressFamily::from_network(prefix.config.prefix);
        let allocation_prefix_len =
            instance_prefix_len(family.ip_address_family(), vpc.config.slaac_enabled);
        if vpc_prefix_can_allocate_interface_prefix(
            family.ip_address_family(),
            prefix.config.prefix.prefix(),
            allocation_prefix_len,
        ) {
            automatic_candidates
                .entry((prefix.vpc_id, family))
                .or_default()
                .push(prefix.id);
        }
    }
    let automatic_candidates: BTreeMap<_, Arc<[VpcPrefixId]>> = automatic_candidates
        .into_iter()
        .map(|(group, candidates)| (group, Arc::from(candidates)))
        .collect();

    Ok(Some(PrefixAllocationContext {
        explicit_prefixes,
        automatic_candidates,
        vpcs,
    }))
}

/// Validates allocation intent and flattens it into canonical work items.
fn plan_prefix_allocations(
    targets: &[NetworkAllocationTarget<'_>],
    context: &PrefixAllocationContext,
) -> CarbideResult<Vec<PrefixAllocationWork>> {
    let PrefixAllocationContext {
        explicit_prefixes,
        automatic_candidates,
        vpcs,
    } = context;

    let mut works = Vec::new();
    // Preserve stable target/interface coordinates while flattening family work.
    for (target_index, target) in targets.iter().enumerate() {
        let mut target_vpc_ids = BTreeSet::new();

        for (interface_index, interface) in target.network_config.interfaces.iter().enumerate() {
            // Automatic intent validates VPC policy before emitting one work
            // item for each requested family.
            if let Some(selection) = &interface.vpc_selection {
                let vpc = vpcs.get(&selection.vpc_id).ok_or_else(|| {
                    CarbideError::internal(format!(
                        "prefix-allocation context is missing automatically selected VPC `{}`",
                        selection.vpc_id,
                    ))
                })?;
                target_vpc_ids.insert(vpc.id);

                if vpc.config.tenant_organization_id != target.tenant_organization_id.to_string() {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "VPC `{}` is not owned by tenant `{}`",
                        vpc.id, target.tenant_organization_id,
                    )));
                }
                if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "automatic VPC-prefix selection requires an FNN VPC; VPC `{}` uses {}",
                        vpc.id, vpc.config.network_virtualization_type,
                    )));
                }

                let families = requested_families(&selection.family_mode);
                for family in families {
                    if !vpc_supports_family(vpc.config.network_virtualization_type, *family) {
                        return Err(CarbideError::FailedPrecondition(format!(
                            "VPC `{}` does not support {} prefixes",
                            vpc.id,
                            family.name(),
                        )));
                    }
                }

                // Resolved dual-stack intent legitimately retains an IPv6
                // sidecar for rolling compatibility, but automatic mode never
                // accepts caller-selected addresses or an unrelated segment.
                if interface.requested_ip_addr.is_some()
                    || interface
                        .ipv6_interface_config
                        .as_ref()
                        .is_some_and(|ipv6| ipv6.requested_ip_addr.is_some())
                    || (interface.network_details.is_none()
                        && interface.ipv6_interface_config.is_some())
                    || (selection.family_mode != InstanceInterfaceIpFamilyMode::DualStack
                        && interface.ipv6_interface_config.is_some())
                    || matches!(
                        interface.network_details.as_ref(),
                        Some(NetworkDetails::NetworkSegment(_))
                    )
                {
                    return Err(CarbideError::InvalidArgument(
                        "explicit IP requests, incompatible IPv6 configuration, and explicit network segments are invalid with automatic VPC-prefix selection"
                            .to_string(),
                    ));
                }
                if !interface_needs_prefix_allocation(interface) {
                    continue;
                }

                for family in families {
                    let allocation_prefix_len =
                        instance_prefix_len(family.ip_address_family(), vpc.config.slaac_enabled);
                    let candidates = automatic_candidates
                        .get(&(selection.vpc_id, *family))
                        .cloned()
                        .unwrap_or_else(|| Arc::from(Vec::<VpcPrefixId>::new()));
                    if candidates.is_empty() {
                        return Err(CarbideError::ResourceExhausted(format!(
                            "VPC `{}` has no eligible {} VPC prefix",
                            selection.vpc_id,
                            family.name(),
                        )));
                    }
                    works.push(PrefixAllocationWork {
                        target_index,
                        interface_index,
                        vpc_id: selection.vpc_id,
                        family: *family,
                        allocation_prefix_len,
                        candidates,
                        candidate_index: 0,
                        // IPv4 creates a dual-stack segment before IPv6 attaches.
                        slot: if families.len() == 2 && *family == AllocationAddressFamily::Ipv6 {
                            PrefixAllocationSlot::SecondaryIpv6
                        } else {
                            PrefixAllocationSlot::Primary
                        },
                        requested_prefix: None,
                        automatic: true,
                    });
                }
                continue;
            }

            // Automatic intent was handled above. Continue only for explicit VPC-prefix
            // selection; segment-backed interfaces require no prefix work.
            let Some(NetworkDetails::VpcPrefixId(primary_prefix_id)) =
                interface.network_details.as_ref()
            else {
                continue;
            };
            let primary_prefix = explicit_prefixes.get(primary_prefix_id).ok_or_else(|| {
                CarbideError::FailedPrecondition(format!(
                    "VPC prefix `{primary_prefix_id}` does not exist",
                ))
            })?;
            let primary_vpc = vpcs.get(&primary_prefix.vpc_id).ok_or_else(|| {
                CarbideError::internal(format!(
                    "prefix-allocation context is missing VPC `{}` referenced by \
                     VPC prefix `{primary_prefix_id}`",
                    primary_prefix.vpc_id
                ))
            })?;
            target_vpc_ids.insert(primary_vpc.id);
            if primary_vpc.config.tenant_organization_id
                != target.tenant_organization_id.to_string()
            {
                return Err(CarbideError::FailedPrecondition(format!(
                    "VPC prefix `{primary_prefix_id}` belongs to VPC `{}`, which is not owned by tenant `{}`",
                    primary_vpc.id, target.tenant_organization_id,
                )));
            }

            let primary_family =
                AllocationAddressFamily::from_network(primary_prefix.config.prefix);
            let primary_allocation_prefix_len = instance_prefix_len(
                primary_family.ip_address_family(),
                primary_vpc.config.slaac_enabled,
            );
            if let Some(requested_ip_addr) = interface.requested_ip_addr
                && AllocationAddressFamily::from_address(requested_ip_addr) != primary_family
            {
                return Err(CarbideError::InvalidArgument(format!(
                    "requested IP address `{requested_ip_addr}` does not match VPC prefix `{primary_prefix_id}`",
                )));
            }
            if primary_vpc.config.slaac_enabled
                && primary_family == AllocationAddressFamily::Ipv6
                && let Some(requested_ip_addr) = interface.requested_ip_addr
            {
                return Err(CarbideError::InvalidArgument(format!(
                    "requested IPv6 address `{requested_ip_addr}` is invalid because VPC `{}` has SLAAC enabled",
                    primary_vpc.id,
                )));
            }
            let secondary_prefix = if let Some(ipv6) = &interface.ipv6_interface_config {
                if primary_family != AllocationAddressFamily::Ipv4 {
                    return Err(CarbideError::InvalidConfiguration(
                        ConfigValidationError::InvalidValue(
                            "vpc_prefix_id points to an IPv6 prefix but ipv6_interface_config is also set -- use one or the other for IPv6"
                                .to_string(),
                        ),
                    ));
                }
                let prefix = explicit_prefixes.get(&ipv6.vpc_prefix_id).ok_or_else(|| {
                    CarbideError::FailedPrecondition(format!(
                        "VPC prefix `{}` does not exist",
                        ipv6.vpc_prefix_id,
                    ))
                })?;
                if !prefix.config.prefix.is_ipv6() {
                    return Err(CarbideError::InvalidArgument(format!(
                        "ipv6_interface_config VPC prefix `{}` is not IPv6",
                        ipv6.vpc_prefix_id,
                    )));
                }
                if prefix.vpc_id != primary_prefix.vpc_id {
                    return Err(CarbideError::InvalidConfiguration(
                        ConfigValidationError::InvalidValue(format!(
                            "dual-stack VPC prefixes must belong to the same VPC: primary_vpc_prefix_id={primary_prefix_id}, primary_vpc_id={}, ipv6_vpc_prefix_id={}, ipv6_vpc_id={}",
                            primary_prefix.vpc_id, ipv6.vpc_prefix_id, prefix.vpc_id,
                        )),
                    ));
                }
                if primary_vpc.config.slaac_enabled
                    && let Some(requested_ip_addr) = ipv6.requested_ip_addr
                {
                    return Err(CarbideError::InvalidArgument(format!(
                        "requested IPv6 address `{requested_ip_addr}` is invalid because VPC `{}` has SLAAC enabled",
                        primary_vpc.id,
                    )));
                }
                Some(prefix)
            } else {
                None
            };

            if !interface_needs_prefix_allocation(interface) {
                continue;
            }

            validate_vpc_prefix_allocation_capacity(
                primary_prefix,
                primary_family,
                primary_allocation_prefix_len,
            )?;

            works.push(PrefixAllocationWork {
                target_index,
                interface_index,
                vpc_id: primary_prefix.vpc_id,
                family: primary_family,
                allocation_prefix_len: primary_allocation_prefix_len,
                candidates: Arc::from(vec![*primary_prefix_id]),
                candidate_index: 0,
                slot: PrefixAllocationSlot::Primary,
                requested_prefix: interface
                    .requested_ip_addr
                    .map(|address| {
                        build_requested_linknet_prefix(address, primary_allocation_prefix_len)
                    })
                    .transpose()?,
                automatic: false,
            });

            if let Some(secondary_prefix) = secondary_prefix {
                let secondary_allocation_prefix_len =
                    instance_prefix_len(IpAddressFamily::Ipv6, primary_vpc.config.slaac_enabled);
                validate_vpc_prefix_allocation_capacity(
                    secondary_prefix,
                    AllocationAddressFamily::Ipv6,
                    secondary_allocation_prefix_len,
                )?;
                let ipv6 = interface.ipv6_interface_config.as_ref().ok_or_else(|| {
                    CarbideError::internal(
                        "validated IPv6 allocation lost its interface configuration".to_string(),
                    )
                })?;
                works.push(PrefixAllocationWork {
                    target_index,
                    interface_index,
                    vpc_id: secondary_prefix.vpc_id,
                    family: AllocationAddressFamily::Ipv6,
                    allocation_prefix_len: secondary_allocation_prefix_len,
                    candidates: Arc::from(vec![secondary_prefix.id]),
                    candidate_index: 0,
                    slot: PrefixAllocationSlot::SecondaryIpv6,
                    requested_prefix: ipv6
                        .requested_ip_addr
                        .map(|address| {
                            build_requested_linknet_prefix(
                                std::net::IpAddr::V6(address),
                                secondary_allocation_prefix_len,
                            )
                        })
                        .transpose()?,
                    automatic: false,
                });
            }
        }

        // Multiple VPCs on one instance are safe only when every attachment
        // uses FNN.
        if target_vpc_ids.len() > 1
            && target_vpc_ids.iter().any(|vpc_id| {
                vpcs.get(vpc_id).is_none_or(|vpc| {
                    vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn
                })
            })
        {
            return Err(CarbideError::InvalidConfiguration(
                ConfigValidationError::InvalidValue(format!(
                    "interface config selects prefixes from multiple VPCs, which is only supported when all VPCs use FNN: {:?}",
                    target_vpc_ids
                        .iter()
                        .filter_map(|vpc_id| {
                            vpcs.get(vpc_id)
                                .map(|vpc| (*vpc_id, vpc.config.network_virtualization_type))
                        })
                        .collect_vec(),
                )),
            ));
        }
    }

    Ok(works)
}

/// Executes planned work in the canonical prefix-lock order.
async fn execute_prefix_allocations(
    targets: &mut [NetworkAllocationTarget<'_>],
    txn: &mut PgConnection,
    mut works: Vec<PrefixAllocationWork>,
) -> CarbideResult<()> {
    // IMPORTANT: Candidate order is part of the locking protocol.
    //
    // Prefix-backed batch work is grouped by VPC and address family, then
    // candidate VPC prefixes are acquired in ascending VpcPrefixId order.
    // Successful row locks live until the outer transaction commits. Changing
    // this to caller order, utilization order, or re-ranking during allocation
    // can invert locks between concurrent batches and introduce deadlocks.
    //
    // For automatic allocation, static first-fit fills the lowest-ID parent
    // prefix before advancing. RESOURCE_EXHAUSTED means the frozen list ended
    // after every candidate was exhausted or became ineligible. Repeated
    // overlap races abort with UNAVAILABLE because capacity remains unproven;
    // later candidate insertions are deferred to the caller's next request.
    //
    // A future throughput-oriented policy could instead rank by utilization and
    // use SKIP LOCKED, selecting the most-used currently unlocked prefix without
    // candidate-lock wait cycles. That policy would spread allocations under
    // contention and must return UNAVAILABLE, not RESOURCE_EXHAUSTED, whenever a
    // skipped candidate leaves capacity unproven. Do not make that trade-off
    // implicitly. Do not refresh or re-sort this transaction's candidate list.
    let mut groups: BTreeMap<(VpcId, AllocationAddressFamily), Vec<usize>> = BTreeMap::new();
    for (work_index, work) in works.iter().enumerate() {
        groups
            .entry((work.vpc_id, work.family))
            .or_default()
            .push(work_index);
    }

    for (_, work_indices) in groups {
        let mut pending: BTreeMap<VpcPrefixId, VecDeque<usize>> = BTreeMap::new();
        for work_index in work_indices {
            let work = works.get(work_index).ok_or_else(|| {
                CarbideError::internal(format!(
                    "prefix allocation work index {work_index} is out of bounds",
                ))
            })?;
            let candidate = work.candidates.first().copied().ok_or_else(|| {
                CarbideError::internal(format!(
                    "prefix allocation work {work_index} has no candidates",
                ))
            })?;
            pending.entry(candidate).or_default().push_back(work_index);
        }

        while let Some((candidate, mut candidate_work)) = pending.pop_first() {
            let mut candidate_unavailable = None;
            while let Some(work_index) = candidate_work.pop_front() {
                if let Some(unavailable) = candidate_unavailable {
                    // Each queued index identifies a distinct `PrefixAllocationWork` for one
                    // interface/address family. A prior work item proved this VPC prefix unavailable
                    // and moved to its next candidate prefix, so move this work to its next candidate
                    // prefix as well without retrying the prefix already known to be unavailable.
                    advance_prefix_work(
                        work_index,
                        candidate,
                        unavailable,
                        &mut works,
                        &mut pending,
                    )?;
                    continue;
                }

                let work = works.get(work_index).cloned().ok_or_else(|| {
                    CarbideError::internal(format!(
                        "prefix allocation work index {work_index} is out of bounds",
                    ))
                })?;
                let operation = match work.slot {
                    PrefixAllocationSlot::Primary => PrefixAllocationOperation::Create,
                    PrefixAllocationSlot::SecondaryIpv6 => {
                        // Canonical grouping runs IPv4 before IPv6 within a VPC,
                        // so the primary must already have created this segment.
                        let target = targets.get(work.target_index).ok_or_else(|| {
                            CarbideError::internal(format!(
                                "network allocation target index {} is out of bounds",
                                work.target_index,
                            ))
                        })?;
                        let interface = target
                            .network_config
                            .interfaces
                            .get(work.interface_index)
                            .ok_or_else(|| {
                                CarbideError::internal(format!(
                                    "network interface index {} is out of bounds for allocation target {}",
                                    work.interface_index, work.target_index,
                                ))
                            })?;
                        let network_segment_id = interface.network_segment_id.ok_or_else(|| {
                            CarbideError::internal(
                                "IPv6 allocation ran before its primary segment was created"
                                    .to_string(),
                            )
                        })?;
                        PrefixAllocationOperation::Attach(network_segment_id)
                    }
                };

                let outcome = attempt_prefix_candidate(
                    txn,
                    work.vpc_id,
                    work.family,
                    work.allocation_prefix_len,
                    candidate,
                    operation,
                    work.requested_prefix,
                )
                .await?;
                match outcome {
                    CandidateAllocationOutcome::Allocated { network_segment_id } => {
                        apply_prefix_resolution(targets, &work, candidate, network_segment_id)?
                    }
                    CandidateAllocationOutcome::Exhausted => {
                        candidate_unavailable = Some(CandidateUnavailable::Exhausted);
                        advance_prefix_work(
                            work_index,
                            candidate,
                            CandidateUnavailable::Exhausted,
                            &mut works,
                            &mut pending,
                        )?;
                    }
                    CandidateAllocationOutcome::Deleted => {
                        candidate_unavailable = Some(CandidateUnavailable::Deleted);
                        advance_prefix_work(
                            work_index,
                            candidate,
                            CandidateUnavailable::Deleted,
                            &mut works,
                            &mut pending,
                        )?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Validates and allocates every target that uses a network prefix in one
/// canonical lock order.
///
/// The function flattens batch work before any generated resource is created so
/// caller order cannot influence the order in which prefix rows are locked.
async fn allocate_networks(
    targets: &mut [NetworkAllocationTarget<'_>],
    txn: &mut PgConnection,
) -> CarbideResult<()> {
    let works = {
        let Some(context) = load_prefix_allocation_context(targets, txn).await? else {
            return Ok(());
        };
        plan_prefix_allocations(targets, &context)?
    };
    execute_prefix_allocations(targets, txn, works).await
}

/// Allocates generated network resources for one instance network config.
pub(crate) async fn allocate_network(
    network_config: &mut InstanceNetworkConfig,
    tenant_organization_id: &TenantOrganizationId,
    txn: &mut PgConnection,
) -> CarbideResult<()> {
    allocate_networks(
        &mut [NetworkAllocationTarget {
            network_config,
            tenant_organization_id,
        }],
        txn,
    )
    .await
}

pub(crate) fn allocate_ib_port_guid(
    ib_config: &InstanceInfinibandConfig,
    machine: &Machine,
) -> CarbideResult<InstanceInfinibandConfig> {
    let mut updated_ib_config = ib_config.clone();

    let ib_hw_info = machine
        .status
        .hardware_info
        .as_ref()
        .ok_or(CarbideError::MissingArgument("no hardware info in machine"))?
        .infiniband_interfaces
        .as_ref();

    // the key of ib_hw_map is device name such as "MT28908 Family [ConnectX-6]".
    // the value of ib_hw_map is a sorted vector of InfinibandInterface by slot.
    let ib_hw_map = sort_ib_by_slot(ib_hw_info);

    let mut guids: Vec<String> = Vec::new();
    for request in &mut updated_ib_config.ib_interfaces {
        tracing::debug!(
            device = %request.device,
            device_instance = request.device_instance,
            "Requested InfiniBand device",
        );

        // TOTO: will support VF in the future. Currently, it will return err when the function_id is not PF.
        if let InterfaceFunctionId::Virtual { .. } = request.function_id {
            return Err(CarbideError::InvalidArgument(format!(
                "not support VF {} (machine {})",
                request.device, machine.id
            )));
        }

        if let Some(sorted_ibs) = ib_hw_map.get(&request.device) {
            if let Some(ib) = sorted_ibs.get(request.device_instance as usize) {
                request.pf_guid = Some(ib.guid.clone());
                request.guid = Some(ib.guid.clone());
                guids.push(ib.guid.clone());
                tracing::debug!(
                    ib_guid = %ib.guid,
                    "select IB device GUID",
                );
            } else {
                return Err(CarbideError::InvalidArgument(format!(
                    "not enough ib device {} (machine {})",
                    request.device, machine.id
                )));
            }
        } else {
            return Err(CarbideError::InvalidArgument(format!(
                "no ib device {} (machine {})",
                request.device, machine.id
            )));
        }
    }

    // Do additional ib ports verification
    if !guids.is_empty() {
        if let Some(ib_interfaces_status) = &machine.status.infiniband_status_observation {
            for guid in guids.iter() {
                for ib_status in ib_interfaces_status.ib_interfaces.iter() {
                    if *guid == ib_status.guid && ib_status.lid == 0xffff_u16 {
                        return Err(CarbideError::InvalidArgument(format!(
                            "UFM detected inactive state for GUID: {guid} (machine {})",
                            machine.id
                        )));
                    }
                }
            }
        } else {
            return Err(CarbideError::InvalidArgument(format!(
                "infiniband status information is not found (machine {})",
                machine.id
            )));
        }
    }

    Ok(updated_ib_config)
}

/// Loads the stored PKeys needed to resolve exact memberships for `configs`.
///
/// A referenced partition without a stored PKey is omitted because it cannot
/// identify an exact membership tuple.
pub(crate) async fn load_ib_partition_pkeys(
    txn: &mut PgConnection,
    configs: &[&InstanceInfinibandConfig],
) -> CarbideResult<HashMap<IBPartitionId, PartitionKey>> {
    let partition_ids = configs
        .iter()
        .flat_map(|config| {
            config
                .ib_interfaces
                .iter()
                .map(|interface| interface.ib_partition_id)
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    if partition_ids.is_empty() {
        return Ok(HashMap::new());
    }

    Ok(db::ib_partition::find_by(
        &mut *txn,
        ObjectColumnFilter::List(ib_partition::IdColumn, &partition_ids),
    )
    .await?
    .into_iter()
    .filter_map(|partition| {
        partition
            .status
            .and_then(|status| status.pkey)
            .map(|pkey| (partition.id, pkey))
    })
    .collect())
}

/// Resolves every exact membership available from one IB config on the
/// supported default fabric.
///
/// Each membership uses the allocated interface GUID and the referenced
/// partition's stored PKey. Memberships are returned in stable database-lock
/// acquisition order. Interfaces missing either value cannot identify a tuple,
/// so they are omitted while other valid memberships are retained.
pub(crate) fn ib_memberships_from_config(
    config: &InstanceInfinibandConfig,
    pkeys: &HashMap<IBPartitionId, PartitionKey>,
) -> BTreeSet<IbMembership> {
    config
        .ib_interfaces
        .iter()
        .filter_map(|interface| {
            Some(IbMembership {
                fabric: DEFAULT_IB_FABRIC_NAME.to_string(),
                pkey: pkeys.get(&interface.ib_partition_id).copied()?,
                guid: interface.guid.clone()?,
            })
        })
        .collect()
}

/// sort ib device by slot and add devices with the same name are added to hashmap
pub(crate) fn sort_ib_by_slot(
    ib_hw_info_vec: &[InfinibandInterface],
) -> HashMap<String, Vec<InfinibandInterface>> {
    let mut ib_hw_map = HashMap::new();
    let mut sorted_ib_hw_info_vec = ib_hw_info_vec.to_owned();
    sorted_ib_hw_info_vec.sort_by_key(|x| match &x.pci_properties {
        Some(pci_properties) => pci_properties.slot.clone().unwrap_or_default(),
        None => "".to_owned(),
    });

    for ib in sorted_ib_hw_info_vec {
        if let Some(ref pci_properties) = ib.pci_properties {
            // description in pci_properties are the value of ID_MODEL_FROM_DATABASE, such as "MT28908 Family [ConnectX-6]"
            if let Some(device) = &pci_properties.description {
                let entry: &mut Vec<InfinibandInterface> =
                    ib_hw_map.entry(device.clone()).or_default();
                entry.push(ib);
            }
        }
    }

    ib_hw_map
}

/// Allocates an instance for a tenant
/// This is a convenience wrapper around `batch_allocate_instances` for single instance allocation.
pub(crate) async fn allocate_instance(
    api: &Api,
    request: InstanceAllocationRequest,
    host_health_config: HostHealthConfig,
) -> Result<ManagedHostStateSnapshot, CarbideError> {
    let mut results = batch_allocate_instances(api, vec![request], host_health_config).await?;

    results
        .pop()
        .ok_or_else(|| CarbideError::internal("instance allocation returned no result".to_string()))
}

/// Loads and locks the extension-service rows and their live versions for
/// `service_ids`, keyed by service ID. Soft-deleted services and versions are
/// left out, so a missing entry reads as "does not exist" to callers.
#[allow(clippy::type_complexity)]
pub(crate) async fn load_extension_services(
    txn: &mut db::Transaction<'_>,
    service_ids: &[ExtensionServiceId],
) -> Result<
    (
        HashMap<ExtensionServiceId, ExtensionService>,
        HashMap<ExtensionServiceId, Vec<ConfigVersion>>,
    ),
    CarbideError,
> {
    let services = extension_service::find_by_ids(txn, service_ids, false, true)
        .await?
        .into_iter()
        .map(|service| (service.id, service))
        .collect();
    let versions = extension_service::find_versions_by_service_ids(txn, service_ids, true).await?;

    Ok((services, versions))
}

/// Validates the durable extension-service configuration an instance is moving to.
///
/// `extension_services` should include both new active and terminating service
/// configs.
///
/// The mixed-path rule is the exception and spans every entry: a terminating
/// attachment still occupies the DPU-agent or the DPF delivery path until its
/// cleanup finishes, so an instance may never straddle both.
pub(crate) fn validate_instance_extension_services(
    machine_id: MachineId,
    is_dpf_managed_host: bool,
    extension_services: &InstanceExtensionServicesConfig,
    services: &HashMap<ExtensionServiceId, ExtensionService>,
    versions: &HashMap<ExtensionServiceId, Vec<ConfigVersion>>,
    existing_active_service_ids: &HashSet<ExtensionServiceId>,
) -> Result<(), CarbideError> {
    let active_services = extension_services.active_services();

    let unique_service_ids: HashSet<_> = active_services
        .iter()
        .map(|config| config.service_id)
        .collect();
    if unique_service_ids.len() != active_services.len() {
        return Err(CarbideError::InvalidArgument(format!(
            "duplicate extension services in configuration. only one version of each service is allowed. (machine {machine_id})"
        )));
    }

    for config in &active_services {
        let service = services.get(&config.service_id).ok_or_else(|| {
            CarbideError::FailedPrecondition(format!(
                "extension service {} does not exist",
                config.service_id,
            ))
        })?;

        // A service type can only be attached to the host model able to
        // reconcile it. DPF Helm services use DPUDevice labels and never reach
        // the DPU agent; Kubernetes Pod services are agent-only. The host flag
        // is authoritative here: the site-wide DPF switch is enforced when a
        // service is created, and no host can be DPF-managed without it.
        match (is_dpf_managed_host, &service.service_type) {
            (true, ExtensionServiceType::KubernetesPod) => {
                return Err(CarbideError::FailedPrecondition(format!(
                    "DPU extension services are not supported on DPF-managed host {machine_id}"
                )));
            }
            (false, ExtensionServiceType::DpfHelmChart) => {
                return Err(CarbideError::FailedPrecondition(format!(
                    "DPF helm chart extension services require a DPF-managed host {machine_id}"
                )));
            }
            _ => {}
        }

        // A DPF Helm chart service is only reconcilable while its DPUService
        // exists. Attachments made before it left Ready stay valid so that a
        // detach is not blocked by the state it is being detached for.
        if service.service_type == ExtensionServiceType::DpfHelmChart
            && !existing_active_service_ids.contains(&config.service_id)
            && service.status.controller_state.value != ExtensionServiceLifecycleState::Ready
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "DPF helm chart extension service {} can only be attached while ready; current state is {:?}",
                config.service_id, service.status.controller_state.value,
            )));
        }

        if !versions
            .get(&config.service_id)
            .is_some_and(|service_versions| service_versions.contains(&config.version))
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "extension service {} version {} does not exist or is deleted",
                config.service_id, config.version,
            )));
        }
    }

    let mut has_dpf_helm_chart = false;
    let mut has_kubernetes_pod = false;
    for config in &extension_services.service_configs {
        // Every referenced service is expected to resolve: deleting one is
        // refused while any live instance still lists it, terminating entries
        // included. An unresolvable entry has no delivery path to account for
        // rather than being worth panicking over.
        match services
            .get(&config.service_id)
            .map(|service| &service.service_type)
        {
            Some(ExtensionServiceType::KubernetesPod) => has_kubernetes_pod = true,
            Some(ExtensionServiceType::DpfHelmChart) => has_dpf_helm_chart = true,
            None => {}
        }
    }
    if has_dpf_helm_chart && has_kubernetes_pod {
        return Err(CarbideError::FailedPrecondition(
            "DPF helm chart and kubernetes pod extension services cannot be attached to the same instance"
                .to_string(),
        ));
    }

    Ok(())
}

fn not_allocatable_error(machine_id: MachineId, reason: NotAllocatableReason) -> CarbideError {
    match reason {
        NotAllocatableReason::InvalidState(state) => CarbideError::InvalidArgument(format!(
            "could not create instance on machine {machine_id} given machine state {state:?}"
        )),
        NotAllocatableReason::PendingInstanceCreation => CarbideError::InvalidArgument(format!(
            "could not create instance on machine {machine_id}. machine is already used by another instance creation request",
        )),
        NotAllocatableReason::PendingBootConfiguration => {
            CarbideError::FailedPrecondition(format!(
                "machine {machine_id} has a pending boot configuration; retry after it has been applied"
            ))
        }
        NotAllocatableReason::NoDpuSnapshots => {
            CarbideError::internal(format!("machine {machine_id} has no DPU. cannot allocate"))
        }
        NotAllocatableReason::MaintenanceMode => CarbideError::MaintenanceMode,
        NotAllocatableReason::HealthAlert(_) => CarbideError::UnhealthyHost,
    }
}

/// Allocates multiple instances in a single transaction.
/// Rolls back entirely if any allocation fails.
///
/// ## Flow:
/// 1. Validate machine types and metadata (in-memory)
/// 2. Batch query machines (FOR UPDATE), load snapshots, validate usability
/// 3. Validate shared resources: NSG, extension services, OS images, IB partitions, DPA
/// 4. Network allocation + config validation (sequential)
/// 5. Batch persist instances, process configs (IPs, IB GUIDs), batch update
/// 6. Load final instances, assemble snapshots, commit
pub(crate) async fn batch_allocate_instances(
    api: &Api,
    mut requests: Vec<InstanceAllocationRequest>,
    host_health_config: HostHealthConfig,
) -> Result<Vec<ManagedHostStateSnapshot>, CarbideError> {
    if requests.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "batch request must contain at least one instance".to_string(),
        ));
    }

    let request_count = requests.len();
    tracing::info!(
        instance_count = request_count,
        "Starting batch instance allocation"
    );

    // ==== Phase 1: Validate request parameters (in-memory validation) ====
    for request in &requests {
        // Validate machine type
        if !request.machine_id.machine_type().is_host() {
            return Err(CarbideError::InvalidArgument(format!(
                "machine with UUID {} is of type {} and can not be converted into an instance",
                request.machine_id,
                request.machine_id.machine_type()
            )));
        }

        // Validate metadata (config validated after network allocation)
        request.metadata.validate(true)?;
    }

    // Start a single transaction for all allocations
    let mut txn = api.txn_begin().await?;

    // ==== Phase 2: Check against allocations for tenants in requests ====

    // To support batching, we'll need to create a unique set of (tenant, instance_type_id)
    // Since we'll filter out any requests that didn't send instance type ID,
    // this means we'll only ever enforce allocation limits when instance type is sent in.
    // That's intentional and allows "targeted" instance creation to bypass allocation enforcement.
    let allocation_validations: HashMap<(&TenantOrganizationId, &InstanceTypeId), usize> = requests
        .iter()
        .filter_map(|request| {
            request.instance_type_id.as_ref().map(|instance_type_id| {
                Ok((
                    &request.config.tenant.tenant_organization_id,
                    instance_type_id,
                ))
            })
        })
        .collect::<Result<Vec<_>, CarbideError>>()?
        .into_iter()
        .counts();

    for ((tenant_organization_id, instance_type_id), req_count) in
        allocation_validations.into_iter()
    {
        // Check that a new instance would not exceed the total allocation count given to this tenant.
        // To do that, we'll need to grab the count of all instances for the tenant,
        // the sum of allocations, and then check that instances.len()+<req_count> is <= allocations_sum.

        // Grab the sum of existing ComputeAllocations for the tenant.
        // We're getting row-level locks on the instance-type and allocations
        // with this.
        let (has_allocations, compute_allocation_total) = {
            let allocs = compute_allocation::sum_allocations(
                &mut txn,
                std::slice::from_ref(instance_type_id),
                Some(tenant_organization_id),
                true,
            )
            .await?
            .get(instance_type_id)
            .copied();

            (allocs.is_some(), allocs.unwrap_or_default())
        };

        // Now we need to grab the count of instances for the tenant for this instance type.
        // We will need to compare the count+1 against new allocation total to make sure a
        // new instance won't exceed it.
        let filter = model::instance::InstanceSearchFilter {
            label: None,
            tenant_org_id: Some(tenant_organization_id.to_string()),
            vpc_id: None,
            instance_type_id: Some(instance_type_id.to_string()),
        };

        // Saturate rather than wrap: an absurd request count then trips the
        // limit check (fail closed) instead of going negative past it.
        let new_total_instance_count = i64::try_from(req_count)
            .unwrap_or(i64::MAX)
            .saturating_add(db::instance::count_ids(&mut txn, filter).await?);

        if new_total_instance_count > i64::from(compute_allocation_total) {
            // # enforce_if_present:  Instance type not required in creation request. If sent and allocations are found for instance type ID, enforce it; otherwise, it's like no limits.
            // # always:              Instance type not required in creation request. If sent, enforce allocations.  If none are found, its a constraint value of 0 (i.e., you get nothing / default-deny).
            // # warn_only (default): Instance type not required in creation request. If sent in and allocations are found, don't enforce, but log what would have happened if they were enforced.
            match (
                has_allocations,
                &api.runtime_config.compute_allocation_enforcement,
            ) {
                (_, ComputeAllocationEnforcement::Always)
                | (true, ComputeAllocationEnforcement::EnforceIfPresent) => {
                    return Err(CarbideError::FailedPrecondition(
                        "request to allocate instance would exceed current tenant allocation limit"
                            .to_string(),
                    ));
                }
                (false, ComputeAllocationEnforcement::EnforceIfPresent) => {
                    tracing::debug!(%tenant_organization_id, %instance_type_id, "EnforceIfPresent set but no allocations seen");
                }
                (_, ComputeAllocationEnforcement::WarnOnly) => {
                    tracing::warn!(%tenant_organization_id, %instance_type_id, "request to allocate instance would exceed current tenant allocation limits if enforcement were enabled");
                }
            }
        }
    }

    // ==== Phase 3: Batch query machines (FOR UPDATE) ====
    let machine_ids: Vec<_> = requests.iter().map(|r| r.machine_id).collect();

    // Grab a row-level locks on the requested machines
    let machines = db::machine::find(
        &mut txn,
        ObjectFilter::List(&machine_ids),
        MachineSearchConfig {
            for_update: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;

    // Create a map for quick lookup
    let machine_map: std::collections::HashMap<_, _> =
        machines.into_iter().map(|m| (m.id, m)).collect();

    // Verify all machines were found
    for request in &requests {
        if !machine_map.contains_key(&request.machine_id) {
            return Err(CarbideError::NotFoundError {
                kind: "Machine",
                id: request.machine_id.to_string(),
            });
        }
    }

    // ==== Phase 4: Batch load managed host snapshots ====
    let mut snapshot_map = db::managed_host::load_by_machine_ids(
        &mut txn,
        &machine_ids,
        LoadSnapshotOptions::default().with_host_health(host_health_config),
    )
    .await?;

    // Attach each machine's DPA interfaces to its snapshot in-place, loaded
    // with a single batched query rather than one query per machine. The ids
    // are sourced from the snapshot map itself (not the request list, which may
    // hold duplicates) so the query keys and the removal keys are the same
    // deduplicated set; each map key is visited exactly once, so `remove` is
    // safe here.
    let dpa_search_config = DpaSearchConfig::default();
    let snapshot_ids: Vec<carbide_uuid::machine::HostMachineId> = snapshot_map
        .values()
        .map(|snapshot| {
            snapshot
                .host_snapshot
                .host_machine_id()
                .map_err(|error| CarbideError::internal(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    let mut dpa_interfaces_by_machine =
        db::dpa_interface::find_by_machine_ids(&mut txn, &snapshot_ids, dpa_search_config).await?;
    for snapshot in snapshot_map.values_mut() {
        let host_machine_id = snapshot
            .host_snapshot
            .host_machine_id()
            .map_err(|error| CarbideError::internal(error.to_string()))?;
        snapshot.dpa_interface_snapshots = dpa_interfaces_by_machine
            .remove(&host_machine_id)
            .unwrap_or_default();
    }

    // Verify all snapshots were loaded and validate usability
    for request in &requests {
        let machine_id = request.machine_id;
        let mh_snapshot = snapshot_map
            .get(&machine_id)
            .ok_or(CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;

        if let Err(e) = mh_snapshot.is_usable_as_instance(request.allow_unhealthy_machine) {
            if matches!(&e, NotAllocatableReason::PendingBootConfiguration) {
                tracing::info!(
                    %machine_id,
                    error = %e,
                    "Host can not be used as instance due to reason",
                );
            } else {
                tracing::error!(
                    %machine_id,
                    error = %e,
                    "Host can not be used as instance due to reason",
                );
            }
            return Err(not_allocatable_error(machine_id, e));
        }
    }

    // ==== Phase 5: Validate shared resources ====

    // Collect all unique NSG IDs with their tenant org IDs for validation
    let nsg_validations: HashSet<_> = requests
        .iter()
        .filter_map(|r| {
            r.config
                .network_security_group_id
                .as_ref()
                .map(|nsg_id| (nsg_id, &r.config.tenant.tenant_organization_id))
        })
        .collect();

    // Validate each unique NSG
    for (nsg_id, tenant_org_id) in &nsg_validations {
        if network_security_group::find_by_ids(
            &mut txn,
            std::slice::from_ref(nsg_id),
            Some(tenant_org_id),
            true,
        )
        .await?
        .pop()
        .is_none()
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "NetworkSecurityGroup `{}` does not exist or is not owned by tenant `{}`",
                nsg_id, tenant_org_id
            )));
        }
    }

    // Collect every requested service ID before resolving them while their
    // service and version rows are locked.
    let service_ids = requests
        .iter()
        .flat_map(|request| request.config.extension_services.service_configs.iter())
        .map(|service| service.service_id)
        .unique()
        .collect_vec();

    if !service_ids.is_empty() {
        let (services, versions) = load_extension_services(&mut txn, &service_ids).await?;

        for request in &requests {
            let mh_snapshot = snapshot_map
                .get(&request.machine_id)
                .expect("requested managed-host snapshot was validated above");
            validate_instance_extension_services(
                request.machine_id,
                mh_snapshot.host_snapshot.config.dpf.used_for_ingestion,
                &request.config.extension_services,
                &services,
                &versions,
                &HashSet::new(),
            )?;
        }
    }

    // Collect all unique OS image IDs for validation
    let os_image_ids: HashSet<_> = requests
        .iter()
        .filter_map(|r| {
            if let OperatingSystemVariant::OsImage(os_image_id) = r.config.os.variant {
                Some(os_image_id)
            } else {
                None
            }
        })
        .collect();

    // Validate each unique OS image
    for os_image_id in &os_image_ids {
        if os_image_id.is_nil() {
            return Err(CarbideError::InvalidArgument(
                "image ID is required for image based storage".to_string(),
            ));
        }
        if let Err(e) = db::os_image::get(&mut txn, *os_image_id).await {
            return if e.is_not_found() {
                Err(CarbideError::FailedPrecondition(format!(
                    "image OS `{}` does not exist",
                    os_image_id
                )))
            } else {
                Err(CarbideError::internal(format!(
                    "failed to get OS image error: {e}"
                )))
            };
        }
    }

    // Validate each OS definition reference is active and READY
    for request in &requests {
        validate_os_definition_usable(&mut txn, &request.config.os).await?;
    }

    // Validate IB partition ownership for all requests
    let ib_partition_validations: Vec<_> = requests
        .iter()
        .flat_map(|r| {
            r.config.infiniband.ib_interfaces.iter().map(|iface| {
                (
                    iface.ib_partition_id,
                    &r.config.tenant.tenant_organization_id,
                )
            })
        })
        .collect();

    batch_validate_ib_partition_ownership(&mut txn, &ib_partition_validations).await?;

    let spx_partition_validations: Vec<_> = requests
        .iter()
        .flat_map(|r| {
            r.config.spxconfig.spx_attachments.iter().map(|attachment| {
                (
                    attachment.spx_partition_id,
                    &r.config.tenant.tenant_organization_id,
                )
            })
        })
        .collect();
    batch_validate_spx_partition_ownership(&mut txn, &spx_partition_validations).await?;

    // Batch query inband segments for all machines
    let inband_segments_map =
        db::instance_network_config::batch_get_inband_segments_by_machine_ids(
            &mut txn,
            &machine_ids,
        )
        .await?;

    // Resolve every interface that uses a network prefix in canonical prefix
    // lock order while preserving caller order for the remaining processing
    // for each instance.
    {
        let mut network_allocation_targets = requests
            .iter_mut()
            .map(|request| {
                let InstanceConfig {
                    network, tenant, ..
                } = &mut request.config;
                NetworkAllocationTarget {
                    network_config: network,
                    tenant_organization_id: &tenant.tenant_organization_id,
                }
            })
            .collect_vec();
        allocate_networks(&mut network_allocation_targets, &mut txn).await?;
    }

    // ==== Phase 5: Network validation and per-instance processing ====
    let mut processed_requests: Vec<(InstanceAllocationRequest, ManagedHostStateSnapshot)> =
        Vec::with_capacity(request_count);

    for request in requests {
        let machine_id = request.machine_id;
        let mh_snapshot = snapshot_map
            .remove(&machine_id)
            .ok_or(CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?;

        // Validate config (after network allocation sets network_segment_id)
        request.config.validate(
            true,
            api.runtime_config
                .vmaas_config
                .as_ref()
                .map(|vc| vc.allow_instance_vf)
                .unwrap_or(true),
        )?;
        validate_instance_vfs_against_dpf_topology(&request.config.network, &api.runtime_config)?;
        validate_instance_interface_routing_profiles(
            &mut txn,
            &request.config.network,
            api.runtime_config.fnn.as_ref(),
        )
        .await?;

        // Zero-DPU hosts (no DPU, or DPU in NIC mode) MUST use `auto`, because
        // their only valid attachments are HostInband segments, and NICo knows
        // which one(s) the host is on. Conversely, hosts with DPUs cannot use
        // `auto`, and are expected to enumerate their interfaces explicitly.
        if !mh_snapshot.has_managed_dpus() {
            let Some(requested_auto_config) = request.config.network.auto_config else {
                return Err(CarbideError::InvalidArgument(format!(
                    "zero-DPU host {} requires `InstanceNetworkConfig.auto = true`; cannot allocate an instance with explicitly-listed interfaces or with `auto = false`",
                    mh_snapshot.host_snapshot.id,
                )));
            };

            validate_zero_dpu_auto_vpc(
                &mut txn,
                requested_auto_config.vpc_id,
                &request.config.tenant.tenant_organization_id,
            )
            .await?;

            // ...and eeven though gRPC <-> model validation rejects
            // auto + non-empty interfaces, double-check here so a future
            // refactor can't silently sneak unsupported segment references
            // past this point. For a zero-DPU host, the only valid
            // attachments are HostInband segments; nothing else can be
            // served by a host with no DPU to handle overlay/tenant
            // networking.
            let allowed_segment_ids: HashSet<_> = mh_snapshot
                .host_snapshot
                .status
                .interfaces
                .iter()
                .filter(|iface| {
                    matches!(
                        iface.network_segment_type,
                        Some(NetworkSegmentType::HostInband)
                    )
                })
                .map(|iface| iface.segment_id)
                .collect();
            for iface in &request.config.network.interfaces {
                if let Some(ns_id) = iface.network_segment_id
                    && !allowed_segment_ids.contains(&ns_id)
                {
                    return Err(CarbideError::InvalidArgument(format!(
                        "zero-DPU host {} cannot serve an instance interface on network segment {ns_id}. must be a HostInband segment only (allowed: {allowed_segment_ids:?})",
                        mh_snapshot.host_snapshot.id,
                    )));
                }
            }

            // HostInband segments may be unbound so multiple Flat VPCs can
            // share the same physical segment. If a segment is still bound,
            // it must not conflict with the VPC requested for this allocation.
            for segment_id in &allowed_segment_ids {
                if let Some(vpc) = db::vpc::find_by_segment(&mut txn, *segment_id).await? {
                    if vpc.id != requested_auto_config.vpc_id {
                        return Err(CarbideError::FailedPrecondition(format!(
                            "zero-DPU host {} has HostInband segment {} bound to VPC {}, but allocation requested VPC {}; shared flat segments must be left unbound",
                            mh_snapshot.host_snapshot.id,
                            segment_id,
                            vpc.id,
                            requested_auto_config.vpc_id,
                        )));
                    }
                    let vpc_iface = vpc
                        .config
                        .network_virtualization_type
                        .fabric_interface_type();
                    if vpc_iface != FabricInterfaceType::Nic {
                        return Err(CarbideError::FailedPrecondition(format!(
                            "zero-DPU host {} has HostInband segment {} bound to VPC {} ({}); zero-DPU hosts can only allocate into VPCs whose fabric_interface_type is `nic` (got `{vpc_iface}`)",
                            mh_snapshot.host_snapshot.id,
                            segment_id,
                            vpc.id,
                            vpc.config.network_virtualization_type,
                        )));
                    }
                }
            }

            // Extension services run on DPU agents; a zero-DPU host has no
            // place to schedule them. We need to check, otherwise the status
            // would just report "Unknown" forever.
            if !request.config.extension_services.service_configs.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "zero-DPU host {} cannot serve extension services; remove `dpu_extension_services` from the instance config",
                    mh_snapshot.host_snapshot.id,
                )));
            }
        } else {
            // `auto` is only valid on zero-DPU hosts; DPU-managed hosts must
            // list their interfaces explicitly.
            if request.config.network.auto_config.is_some() {
                return Err(CarbideError::InvalidArgument(format!(
                    "host {} has DPUs; `InstanceNetworkConfig.auto` is only valid on zero-DPU hosts",
                    mh_snapshot.host_snapshot.id,
                )));
            }

            // DPU-managed hosts must only allocate into VPCs whose
            // fabric interface type matches (i.e. `Dpu`). The segment-
            // binding rule already prevents `HostInband` segments from
            // living in a Dpu-fabric VPC, but reject explicitly here so
            // a DPU instance referencing a `HostInband` segment (which
            // would be in a Nic-fabric VPC) fails with a clear message
            // rather than getting stuck somewhere downstream.
            for iface in &request.config.network.interfaces {
                if let Some(ns_id) = iface.network_segment_id {
                    match db::vpc::find_by_segment(&mut txn, ns_id)
                        .await
                        .map_err(CarbideError::from)?
                    {
                        Some(vpc) => {
                            let vpc_iface = vpc
                                .config
                                .network_virtualization_type
                                .fabric_interface_type();
                            if vpc_iface != FabricInterfaceType::Dpu {
                                return Err(CarbideError::FailedPrecondition(format!(
                                    "DPU-managed host {} cannot allocate an instance into VPC {} ({}, via segment {}); DPU hosts can only allocate into VPCs whose fabric_interface_type is `dpu` (got `{vpc_iface}`)",
                                    mh_snapshot.host_snapshot.id,
                                    vpc.id,
                                    vpc.config.network_virtualization_type,
                                    ns_id,
                                )));
                            }
                        }
                        None => {
                            return Err(CarbideError::FailedPrecondition(format!(
                                "DPU-managed host {} cannot allocate an instance into network segment {}; DPU allocations require the segment to be attached to a VPC",
                                mh_snapshot.host_snapshot.id, ns_id,
                            )));
                        }
                    }
                }
            }
        }

        processed_requests.push((request, mh_snapshot));
    }

    // ==== Phase 6: Batch persist instances ====
    let network_config_version = ConfigVersion::initial();
    let ib_config_version = ConfigVersion::initial();
    let extension_services_config_version = ConfigVersion::initial();
    let config_version = ConfigVersion::initial();
    let nvl_config_version = ConfigVersion::initial();
    let spx_config_version = ConfigVersion::initial();

    let new_instances: Vec<NewInstance<'_>> = processed_requests
        .iter()
        .map(|(request, _)| NewInstance {
            instance_id: request.instance_id,
            instance_type_id: request.instance_type_id.clone(),
            machine_id: request.machine_id,
            config: &request.config,
            metadata: request.metadata.clone(),
            config_version,
            network_config_version,
            ib_config_version,
            extension_services_config_version,
            nvlink_config_version: nvl_config_version,
            spx_config_version,
        })
        .collect();

    let _persisted_instances = db::instance::batch_persist(new_instances, &mut txn).await?;

    // ==== Phase 7: Process configs (IPs, inband interfaces, IB GUIDs) ====
    // These need to be done per-instance but we collect results for batch update
    // Tuple format: (instance_id, expected_version, config)
    let mut network_config_updates: Vec<(
        carbide_uuid::instance::InstanceId,
        ConfigVersion,
        model::instance::config::network::InstanceNetworkConfig,
    )> = Vec::with_capacity(request_count);
    let mut ib_config_updates: Vec<(
        carbide_uuid::instance::InstanceId,
        ConfigVersion,
        model::instance::config::infiniband::InstanceInfinibandConfig,
    )> = Vec::with_capacity(request_count);
    let mut nvlink_config_updates: Vec<(
        carbide_uuid::instance::InstanceId,
        ConfigVersion,
        model::instance::config::nvlink::InstanceNvLinkConfig,
    )> = Vec::with_capacity(request_count);
    let mut spx_config_updates: Vec<(
        carbide_uuid::instance::InstanceId,
        ConfigVersion,
        model::instance::config::spx::InstanceSpxConfig,
    )> = Vec::with_capacity(request_count);

    for (request, mh_snapshot) in &processed_requests {
        let instance_id = request.instance_id;

        // Add host-inband network segments (using pre-queried batch data)
        let inband_segment_ids = inband_segments_map
            .get(&mh_snapshot.host_snapshot.id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let updated_network_config = db::instance_network_config::add_inband_interfaces_to_config(
            request.config.network.clone(),
            inband_segment_ids,
        )?;

        // Allocate IPs
        let updated_network_config = db::instance_network_config::with_allocated_ips(
            updated_network_config,
            &mut txn,
            instance_id,
            &mh_snapshot.host_snapshot,
        )
        .await?;

        if updated_network_config.interfaces.is_empty() {
            return Err(CarbideError::InvalidConfiguration(
                ConfigValidationError::InvalidValue(format!(
                    "InstanceNetworkConfig.interfaces is empty (machine {})",
                    request.machine_id
                )),
            ));
        }

        network_config_updates.push((instance_id, network_config_version, updated_network_config));

        // Allocate IB GUID
        let updated_ib_config =
            allocate_ib_port_guid(&request.config.infiniband, &mh_snapshot.host_snapshot)?;
        ib_config_updates.push((instance_id, ib_config_version, updated_ib_config));

        // NVLink config
        nvlink_config_updates.push((
            instance_id,
            nvl_config_version,
            request.config.nvlink.clone(),
        ));

        let updated_spx_config = allocate_spx_port_mac(&request.config.spxconfig, mh_snapshot)?;
        spx_config_updates.push((instance_id, spx_config_version, updated_spx_config));
    }

    // ==== Phase 8: Batch update configs ====
    // increment_version = false: during initial creation, we don't increment
    let network_refs: Vec<_> = network_config_updates
        .iter()
        .map(|(id, ver, cfg)| (*id, *ver, cfg))
        .collect();
    db::instance::batch_update_network_config(&mut txn, &network_refs, false).await?;

    let ib_refs: Vec<_> = ib_config_updates
        .iter()
        .map(|(id, ver, cfg)| (*id, *ver, cfg))
        .collect();
    let ib_configs = ib_config_updates
        .iter()
        .map(|(_, _, config)| config)
        .collect::<Vec<_>>();
    let ib_partition_pkeys = load_ib_partition_pkeys(txn.as_mut(), &ib_configs).await?;
    let assigned_ib_memberships = ib_configs
        .into_iter()
        .flat_map(|config| ib_memberships_from_config(config, &ib_partition_pkeys))
        .collect::<BTreeSet<_>>();
    db::instance::batch_update_ib_config(&mut txn, &ib_refs, false).await?;
    // The Machine records are still locked here. Remove an exact retired record
    // only after its live config write succeeds in this same transaction.
    for membership in assigned_ib_memberships {
        db::retired_ib_membership::remove_for_reuse(txn.as_mut(), &membership).await?;
    }

    let nvlink_refs: Vec<_> = nvlink_config_updates
        .iter()
        .map(|(id, ver, cfg)| (*id, *ver, cfg))
        .collect();
    db::instance::batch_update_nvlink_config(&mut txn, &nvlink_refs, false).await?;

    let spx_refs: Vec<_> = spx_config_updates
        .iter()
        .map(|(id, ver, cfg)| (*id, *ver, cfg))
        .collect();
    db::instance::batch_update_spx_config(&mut txn, &spx_refs, false).await?;

    // ==== Phase 9: Load final instances ====
    let machine_id_refs: Vec<&MachineId> = processed_requests
        .iter()
        .map(|(r, _)| &r.machine_id)
        .collect();
    let final_instances = db::instance::find_by_machine_ids(&mut txn, &machine_id_refs).await?;
    let mut final_instance_map: HashMap<_, _> = final_instances
        .into_iter()
        .map(|i| (i.machine_id, i))
        .collect();

    // ==== Phase 10: Assemble final snapshots ====
    let mut snapshots = Vec::with_capacity(request_count);
    for (request, mut mh_snapshot) in processed_requests {
        let machine_id = request.machine_id;
        mh_snapshot.instance = Some(final_instance_map.remove(&machine_id).ok_or_else(|| {
            CarbideError::internal(format!(
                "newly created instance for {machine_id} was not found"
            ))
        })?);
        snapshots.push(mh_snapshot);
    }

    // ==== Phase 11: Commit ====
    txn.commit().await?;

    tracing::info!(
        instance_count = snapshots.len(),
        "Successfully completed batch instance allocation"
    );

    Ok(snapshots)
}

/// Batch validate SPX partition ownership for multiple (partition_id, tenant_id) pairs
pub(crate) async fn batch_validate_spx_partition_ownership(
    txn: &mut PgConnection,
    validations: &[(SpxPartitionId, &TenantOrganizationId)],
) -> CarbideResult<()> {
    if validations.is_empty() {
        tracing::info!("batch_validate_spx_partition_ownership validations is empty");
        return Ok(());
    }

    // Batch query all unique partitions
    let unique_partition_ids: Vec<_> = validations
        .iter()
        .map(|(id, _)| *id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let partitions = db::spx_partition::find_by(
        txn,
        ObjectColumnFilter::List(db::spx_partition::IdColumn, &unique_partition_ids),
    )
    .await?;

    let partition_map: HashMap<_, _> = partitions.into_iter().map(|p| (p.id, p)).collect();

    // Validate each partition ownership
    for (partition_id, expected_tenant) in validations {
        let partition = partition_map.get(partition_id).ok_or_else(|| {
            tracing::error!(
                spx_partition_id = %partition_id,
                "SPX partition not found while validating ownership",
            );
            ConfigValidationError::invalid_value(format!(
                "SPX partition {partition_id} is not created"
            ))
        })?;

        if &partition.tenant_organization_id != *expected_tenant {
            tracing::error!(
                spx_partition_id = %partition_id,
                "SPX partition is not owned by the tenant",
            );
            return Err(CarbideError::InvalidArgument(format!(
                "SPX partition {partition_id} is not owned by the tenant {expected_tenant}",
            )));
        }
    }
    Ok(())
}

/// Batch validate IB partition ownership for multiple (partition_id, tenant_id) pairs
pub(crate) async fn batch_validate_ib_partition_ownership(
    txn: &mut PgConnection,
    validations: &[(IBPartitionId, &TenantOrganizationId)],
) -> CarbideResult<()> {
    if validations.is_empty() {
        return Ok(());
    }

    // Batch query all unique partitions
    let unique_partition_ids: Vec<_> = validations
        .iter()
        .map(|(id, _)| *id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let partitions = db::ib_partition::find_by(
        txn,
        ObjectColumnFilter::List(ib_partition::IdColumn, &unique_partition_ids),
    )
    .await?;

    let partition_map: HashMap<_, _> = partitions.into_iter().map(|p| (p.id, p)).collect();

    // Validate each partition ownership
    for (partition_id, expected_tenant) in validations {
        let partition = partition_map.get(partition_id).ok_or_else(|| {
            ConfigValidationError::invalid_value(format!(
                "IB partition {partition_id} is not created"
            ))
        })?;

        if &partition.config.tenant_organization_id != *expected_tenant {
            return Err(CarbideError::InvalidArgument(format!(
                "IB partition {partition_id} is not owned by the tenant {expected_tenant}",
            )));
        }
    }
    Ok(())
}

/// Check whether the tenant of instance is consistent with the tenant of the ib partition
pub(crate) async fn validate_ib_partition_ownership(
    txn: &mut PgConnection,
    instance_tenant: &TenantOrganizationId,
    ib_config: &InstanceInfinibandConfig,
) -> CarbideResult<()> {
    let validations: Vec<_> = ib_config
        .ib_interfaces
        .iter()
        .map(|iface| (iface.ib_partition_id, instance_tenant))
        .collect();
    batch_validate_ib_partition_ownership(txn, &validations).await
}

pub(crate) async fn validate_spx_partition_ownership(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    instance_tenant: &TenantOrganizationId,
    spxcfg: &InstanceSpxConfig,
) -> Result<(), CarbideError> {
    for attachment in &spxcfg.spx_attachments {
        let partition_id = attachment.spx_partition_id;

        let partition = db::spx_partition::find_by(
            txn.as_mut(),
            ObjectColumnFilter::List(db::spx_partition::IdColumn, &[partition_id]),
        )
        .await?;
        if partition.len() != 1 {
            return Err(CarbideError::InvalidArgument(format!(
                "SPX partition {partition_id} is not found",
            )));
        }
        let spx_partition = &partition[0];
        if spx_partition.tenant_organization_id != *instance_tenant {
            return Err(CarbideError::InvalidArgument(format!(
                "SPX partition {partition_id} is not owned by the tenant {instance_tenant}",
            )));
        }
    }

    Ok(())
}

/// sort spx device by slot and add devices with the same name are added to hashmap
pub(crate) fn sort_spx_by_slot(
    spx_hw_info_vec: &[DpaInterface],
) -> HashMap<String, Vec<DpaInterface>> {
    let mut spx_hw_map = HashMap::new();
    let mut sorted_spx_hw_info_vec = spx_hw_info_vec.to_owned();
    sorted_spx_hw_info_vec.sort_by(|a, b| a.pci_name.cmp(&b.pci_name));

    for spx in sorted_spx_hw_info_vec {
        if let Some(device) = &spx.device_description.clone() {
            let entry: &mut Vec<DpaInterface> = spx_hw_map.entry(device.clone()).or_default();
            entry.push(spx);
        } else {
            tracing::info!(
                spx = ?spx,
                "SPX device description is missing",
            );
        }
    }

    spx_hw_map
}

/// Allocate SPX port MAC addresses
pub(crate) fn allocate_spx_port_mac(
    spx_config: &InstanceSpxConfig,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> CarbideResult<InstanceSpxConfig> {
    let mut updated_spx_config = spx_config.clone();

    tracing::debug!(
        dpa_interface_snapshot_count = mh_snapshot.dpa_interface_snapshots.len(),
        "Allocating SPX port MAC addresses",
    );

    let mut seen_device_instances = HashSet::new();
    for att in &updated_spx_config.spx_attachments {
        if !seen_device_instances.insert((att.device.clone(), att.device_instance)) {
            tracing::error!(
                device = %att.device,
                device_instance = att.device_instance,
                "Duplicate SPX attachment",
            );
            return Err(CarbideError::InvalidArgument(format!(
                "duplicate SPX attachment for device {} instance {}",
                att.device, att.device_instance,
            )));
        }
    }

    // Process higher `device_instance` indices first so removing a consumed interface from
    // `sorted_spxs` does not shift indices still needed for lower instances on the same device.
    updated_spx_config
        .spx_attachments
        .sort_unstable_by(|a, b| match a.device.cmp(&b.device) {
            Ordering::Equal => b.device_instance.cmp(&a.device_instance),
            o => o,
        });

    let mut spx_hw_map = sort_spx_by_slot(mh_snapshot.dpa_interface_snapshots.as_ref());

    for spx_attachment in &mut updated_spx_config.spx_attachments {
        if spx_attachment.attachment_type == SpxAttachmentType::Virtual {
            tracing::error!("allocate_spx_port_mac SPX attachment type Virtual is not supported");
            return Err(CarbideError::InvalidArgument(
                "SPX attachment type virtual is not supported".to_string(),
            ));
        }
        if let Some(sorted_spxs) = spx_hw_map.get_mut(&spx_attachment.device) {
            if let Some(spx_interface) = sorted_spxs.get(spx_attachment.device_instance as usize) {
                spx_attachment.mac_address = Some(spx_interface.mac_address.to_string());
                sorted_spxs.remove(spx_attachment.device_instance as usize);
            } else {
                tracing::error!(
                    device = %spx_attachment.device,
                    device_instance = spx_attachment.device_instance,
                    "SPX device has no matching instance",
                );
                return Err(CarbideError::InvalidArgument(format!(
                    "SPX device {} has no instance {}",
                    spx_attachment.device, spx_attachment.device_instance,
                )));
            }
        } else {
            tracing::error!(
                device = %spx_attachment.device,
                machine_id = %mh_snapshot.host_snapshot.id,
                "SPX device not found",
            );
            return Err(CarbideError::InvalidArgument(format!(
                "no SPX device with name {} in machine {}",
                spx_attachment.device, mh_snapshot.host_snapshot.id,
            )));
        }
    }

    Ok(updated_spx_config)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases, check_values, value_scenarios};
    use model::instance::config::infiniband::InstanceIbInterfaceConfig;

    use super::*;

    /// Test-specific helper that builds one allocated physical IB interface.
    fn ib_interface(partition_id: IBPartitionId, guid: Option<&str>) -> InstanceIbInterfaceConfig {
        InstanceIbInterfaceConfig {
            function_id: InterfaceFunctionId::Physical {},
            ib_partition_id: partition_id,
            pf_guid: guid.map(str::to_string),
            guid: guid.map(str::to_string),
            device: "test-device".to_string(),
            vendor: None,
            device_instance: 0,
        }
    }

    #[test]
    fn ib_memberships_preserve_every_resolvable_tuple() {
        let valid_partition = IBPartitionId::new();
        let missing_pkey_partition = IBPartitionId::new();
        let missing_guid_partition = IBPartitionId::new();
        let valid_pkey = PartitionKey::try_from(0x101).unwrap();
        let missing_guid_pkey = PartitionKey::try_from(0x102).unwrap();
        let config = InstanceInfinibandConfig {
            ib_interfaces: vec![
                ib_interface(valid_partition, Some("valid-guid")),
                ib_interface(missing_pkey_partition, Some("unknown-pkey-guid")),
                ib_interface(missing_guid_partition, None),
            ],
        };

        // Retirement bookkeeping can act only on exact tuples. Incomplete
        // interfaces are skipped without discarding another valid membership.
        assert_eq!(
            ib_memberships_from_config(
                &config,
                &HashMap::from([
                    (valid_partition, valid_pkey),
                    (missing_guid_partition, missing_guid_pkey),
                ]),
            ),
            BTreeSet::from([IbMembership {
                fabric: DEFAULT_IB_FABRIC_NAME.to_string(),
                pkey: valid_pkey,
                guid: "valid-guid".to_string(),
            }])
        );
    }

    #[test]
    fn interface_needs_prefix_allocation_only_without_durable_results() {
        struct AllocationState {
            has_ip_address: bool,
            has_interface_prefix: bool,
        }

        check_values(
            [
                Check {
                    scenario: "no address or interface prefix needs allocation",
                    input: AllocationState {
                        has_ip_address: false,
                        has_interface_prefix: false,
                    },
                    expect: true,
                },
                Check {
                    scenario: "an allocated address is durable",
                    input: AllocationState {
                        has_ip_address: true,
                        has_interface_prefix: false,
                    },
                    expect: false,
                },
                Check {
                    scenario: "an allocated interface prefix is durable",
                    input: AllocationState {
                        has_ip_address: false,
                        has_interface_prefix: true,
                    },
                    expect: false,
                },
                Check {
                    scenario: "an allocated address and interface prefix are durable",
                    input: AllocationState {
                        has_ip_address: true,
                        has_interface_prefix: true,
                    },
                    expect: false,
                },
            ],
            |AllocationState {
                 has_ip_address,
                 has_interface_prefix,
             }| {
                let mut interface =
                    InstanceNetworkConfig::for_vpc_prefix_id(VpcPrefixId::new(), None)
                        .interfaces
                        .into_iter()
                        .next()
                        .unwrap();
                let network_prefix_id = carbide_uuid::network::NetworkPrefixId::new();

                if has_ip_address {
                    interface
                        .ip_addrs
                        .insert(network_prefix_id, "192.0.2.10".parse().unwrap());
                }
                if has_interface_prefix {
                    let interface_prefix = if has_ip_address {
                        "192.0.2.10/32"
                    } else {
                        "2001:db8::/127"
                    };
                    interface
                        .interface_prefixes
                        .insert(network_prefix_id, interface_prefix.parse().unwrap());
                }

                interface_needs_prefix_allocation(&interface)
            },
        );
    }

    #[test]
    fn instance_creation_power_profile_semantics() {
        value_scenarios!(
            run = normalize_created_power_profile;
            "non-empty profile is preserved" {
                Some("balanced".to_string()) => Some("balanced".to_string()),
            }
            "empty profile is treated as unset" {
                Some(String::new()) => None,
            }
            "omitted profile remains unset" {
                None => None,
            }
        );
    }

    #[test]
    fn build_requested_linknet_prefix_accepts_host_end_rejects_dpu_end() {
        // The host must take the odd (::1) end of the linknet; the even end is the
        // DPU's and is rejected. `CarbideError` isn't `PartialEq` (so it can't be the
        // table's error type): error rows only assert *that* it fails via `Fails`, and
        // the closure discards the error to `()` to satisfy `check_cases`.
        check_cases(
            [
                Case {
                    scenario: "host end of a /31 (odd v4)",
                    input: ("10.0.0.1", 31),
                    expect: Yields("10.0.0.1/31".parse().unwrap()),
                },
                Case {
                    scenario: "host end of a /127 (::1 v6)",
                    input: ("2001:db8::1", 127),
                    expect: Yields("2001:db8::1/127".parse().unwrap()),
                },
                Case {
                    scenario: "DPU end of a /31 (even v4) is rejected",
                    input: ("10.0.0.0", 31),
                    expect: Fails,
                },
                Case {
                    scenario: "DPU end of a /127 (::0 v6) is rejected",
                    input: ("2001:db8::0", 127),
                    expect: Fails,
                },
            ],
            |(ip, prefix_len)| {
                build_requested_linknet_prefix(ip.parse().unwrap(), prefix_len).map_err(|_| ())
            },
        );
    }

    /// Verifies instance admission uses exact topology membership without changing legacy mode.
    #[test]
    fn instance_vf_admission_follows_dpf_topology() {
        #[derive(Clone, Copy)]
        enum InventoryMode {
            Topology(&'static [u8]),
            Static,
            DpfDisabled(&'static [u8]),
        }

        value_scenarios!(
            run = |(mode, requested_vfs)| {
                let config = match mode {
                    InventoryMode::Topology(vf_ids) => {
                        crate::test_support::default_config::with_dpf_intercept_topology(vf_ids)
                    }
                    InventoryMode::Static => {
                        let mut config = crate::test_support::default_config::get();
                        config.dpf.enabled = true;
                        config.vmaas_config = None;
                        config
                    }
                    InventoryMode::DpfDisabled(vf_ids) => {
                        let mut config =
                            crate::test_support::default_config::with_dpf_intercept_topology(vf_ids);
                        config.dpf.enabled = false;
                        config
                    }
                };
                validate_vf_ids_against_dpf_topology(requested_vfs, &config).is_ok()
            };
            "selected sparse VF" {
                // An explicitly selected sparse VF is addressable.
                (InventoryMode::Topology(&[7]), vec![7]) => true,
            }

            "unselected sparse VF" {
                // VF0 must not pass merely because the hardware provisions it.
                (InventoryMode::Topology(&[7]), vec![0]) => false,
            }

            "all requested VFs selected" {
                // Multiple requested VFs must each belong to the replacement inventory.
                (InventoryMode::Topology(&[4, 7]), vec![4, 7]) => true,
            }

            "one requested VF omitted" {
                // One unselected VF rejects the complete network configuration.
                (InventoryMode::Topology(&[4, 7]), vec![4, 6]) => false,
            }

            "PF-only topology" {
                // A valid PF-only topology deliberately exposes no instance VFs.
                (InventoryMode::Topology(&[]), vec![0]) => false,
            }

            "static compatibility mode" {
                // DPF without a replacement topology retains the historical admission behavior.
                (InventoryMode::Static, vec![0]) => true,
            }

            "DPF disabled" {
                // Legacy non-DPF VMaaS configuration remains outside this DPF admission gate.
                (InventoryMode::DpfDisabled(&[7]), vec![0]) => true,
            }
        );
    }

    #[test]
    fn pending_boot_configuration_has_a_safe_allocation_error() {
        let machine_id = "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30"
            .parse()
            .unwrap();

        assert!(matches!(
            not_allocatable_error(
                machine_id,
                NotAllocatableReason::PendingBootConfiguration,
            ),
            CarbideError::FailedPrecondition(message)
                if message
                    == format!(
                        "machine {machine_id} has a pending boot configuration; retry after it has been applied"
                    )
        ));
    }
}

#[cfg(test)]
#[test]
fn test_sort_ib_by_slot() {
    let data = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../api-model/src/hardware_info/test_data/x86_info.json"
    ));

    let hw_info = serde_json::from_slice::<model::hardware_info::HardwareInfo>(data).unwrap();
    assert!(!hw_info.infiniband_interfaces.is_empty());

    let prev = sort_ib_by_slot(hw_info.infiniband_interfaces.as_ref());
    for _ in 0..10 {
        let cur = sort_ib_by_slot(hw_info.infiniband_interfaces.as_ref());
        for (key, value) in cur.into_iter() {
            assert_eq!(*prev.get(&key).unwrap(), value);
        }
    }
}

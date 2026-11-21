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
use std::time::Duration;

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_resource_pool::remote::RemoteResourcePoolError;
use carbide_resource_pool::{ResourcePoolBackendError, database_transaction};
use carbide_uuid::domain::DomainId;
use carbide_uuid::vpc::VpcId;
use db::db_read::PgPoolReader;
use db::dns::domain;
use db::network_segment::reconcile_network_defs;
use db::vpc::{self, VpcRowLock};
use db::work_lock_manager::{AcquireLockError, WorkLock};
use db::{ObjectColumnFilter, TransactionVending as _, dpu_agent_upgrade_policy, network_segment};
use itertools::Itertools;
use model::dns::{Domain, NewDomain};
use model::firmware::AgentUpgradePolicyChoice;
use model::machine::upgrade_policy::AgentUpgradePolicy;
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{NetworkDefinition, NetworkSegmentType, NewNetworkSegment};
use model::resource_pool::OwnerType;
use model::vpc::{NewVpc, VpcDefinition, VpcStatus, VpcVirtualizationTypeCapabilities};
use sqlx::{Pool, Postgres};

use crate::CarbideError;
use crate::api::Api;
use crate::handlers::network_segment::{
    SEGMENT_COMPLETION_TIMEOUT, SegmentAllocations, SegmentPreparationError,
    validate_segment_persistence,
};

const STATIC_ASSIGNMENTS_IPV4_PREFIX: &str = "169.254.254.254/32";
const STATIC_ASSIGNMENTS_IPV6_PREFIX: &str = "100::/128";

/// Creates the configured initial domain when the `domains` table is empty.
///
/// Returns `true` only if this call inserts the domain. Existing rows, even
/// deleted ones, return `false`.
pub(crate) async fn create_initial_domain(
    db_pool: sqlx::pool::Pool<Postgres>,
    domain_name: &str,
) -> Result<bool, CarbideError> {
    let mut txn = db_pool.txn_begin().await?;
    let domains = domain::find_by(&mut txn, ObjectColumnFilter::<domain::IdColumn>::All).await?;
    if domains.is_empty() {
        let domain = NewDomain::new(domain_name);
        let created = db::dns::domain::persist_first(&domain, &mut txn)
            .await?
            .is_some();
        txn.commit().await?;
        Ok(created)
    } else {
        let names: Vec<String> = domains.into_iter().map(|d| d.name).collect();
        if !names.iter().any(|n| n == domain_name) {
            tracing::warn!(
                domain_name,
                domains = ?names,
                "Initial domain name in config file does not match existing database domains",
            );
        }
        Ok(false)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum InitialNetworkDomainSelection {
    Selected(DomainId),
    NoForwardDomain,
    Ambiguous(Vec<String>),
}

/// Select the forward domain used by configured initial network segments.
///
/// Reverse-DNS zones share the domains table and must not make a sole forward
/// domain appear ambiguous.
fn select_initial_network_domain(
    domains: &[Domain],
    configured_domain_name: Option<&str>,
) -> InitialNetworkDomainSelection {
    let forward_domains = domains
        .iter()
        .filter(|domain| {
            let name = domain.name.trim_end_matches('.');
            !matches!(name, "in-addr.arpa" | "ip6.arpa")
                && db::dns::normalize_reverse_zone_name(name).is_none()
        })
        .collect_vec();

    if let Some(domain_name) = configured_domain_name {
        let configured_domains = forward_domains
            .iter()
            .filter(|domain| domain.name == domain_name)
            .collect_vec();
        if let [domain] = configured_domains.as_slice() {
            return InitialNetworkDomainSelection::Selected(domain.id);
        }
    }

    match forward_domains.as_slice() {
        [] => InitialNetworkDomainSelection::NoForwardDomain,
        [domain] => InitialNetworkDomainSelection::Selected(domain.id),
        domains => InitialNetworkDomainSelection::Ambiguous(
            domains
                .iter()
                .map(|domain| domain.name.clone())
                .sorted()
                .collect(),
        ),
    }
}

pub(crate) async fn create_initial_networks(
    api: &Api,
    db_pool: &Pool<Postgres>,
    networks: &HashMap<String, NetworkDefinition>,
) -> Result<(), CarbideError> {
    let domains = db::dns::domain::find_by(
        &mut api.db_reader(),
        ObjectColumnFilter::<db::dns::domain::IdColumn>::All,
    )
    .await?;
    let domain_id = match select_initial_network_domain(
        &domains,
        api.runtime_config.initial_domain_name.as_deref(),
    ) {
        InitialNetworkDomainSelection::Selected(domain_id) => domain_id,
        InitialNetworkDomainSelection::NoForwardDomain => {
            tracing::warn!("No forward domain configured, skipping initial network creation");
            return Ok(());
        }
        InitialNetworkDomainSelection::Ambiguous(forward_domains) => {
            tracing::warn!(
                ?forward_domains,
                configured_domain_name = ?api.runtime_config.initial_domain_name,
                "Multiple forward domains, skipping initial network creation",
            );
            return Ok(());
        }
    };
    let externally_prepared = networks
        .values()
        .any(|def| SegmentAllocations::requires_external_preparation(api, def.segment_type.into()));
    if !externally_prepared {
        return create_initial_network_batch(api, db_pool, networks, domain_id).await;
    }
    // Reject invalid retained routing before making any independent commits.
    // Each new segment checks its own direct-prefix admissibility below.
    let mut txn = db_pool.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(api, &mut txn)
        .await?;
    txn.commit().await?;
    // Each prepared pair reaches its entity transaction before we ask for the
    // next segment. Earlier commits survive a later provider or SQL failure.
    for (name, def) in networks.iter().sorted_by_key(|(name, _)| *name) {
        create_initial_network_individually(api, name, def, domain_id).await?;
    }
    let mut txn = db_pool.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    db::network_segment::diagnose_dropped_network_defs(&mut txn, networks, &api.instrumentation)
        .await?;
    repair_initial_network_dns(api, &mut txn, networks, domain_id).await?;
    crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(api, &mut txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

/// Entity-transaction allocators retain startup's original whole-batch rollback.
async fn create_initial_network_batch(
    api: &Api,
    db_pool: &Pool<Postgres>,
    networks: &HashMap<String, NetworkDefinition>,
    domain_id: DomainId,
) -> Result<(), CarbideError> {
    let mut txn = db_pool.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    let mut to_create = reconcile_network_defs(&mut txn, networks, &api.instrumentation).await?;
    to_create.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    for (name, def) in &to_create {
        let ns = NewNetworkSegment::build_from(name, domain_id, def)?;
        let (ns, _) = configure_segment_vpc(&mut txn, ns, def, VpcRowLock::None).await?;
        let allocations = SegmentAllocations::entity_transaction(&ns);
        // Whole-batch startup allocates SVI addresses in update_network_segments_svi_ip.
        persist_initial_network(api, &mut txn, name, def, ns, &allocations, false).await?;
    }
    repair_initial_network_dns(api, &mut txn, networks, domain_id).await?;
    crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(api, &mut txn)
        .await?;
    txn.commit().await?;
    for (name, _) in to_create {
        tracing::info!(network_segment_name = %name, "Created network segment");
    }
    Ok(())
}

async fn persist_initial_network(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    name: &str,
    def: &NetworkDefinition,
    ns: NewNetworkSegment,
    allocations: &SegmentAllocations<'_>,
    allocate_svi_ip: bool,
) -> Result<model::network_segment::NetworkSegment, CarbideError> {
    let segment_id = ns.id;
    tracing::info!(
        network_segment_name = name,
        network_segment = ?ns,
        "Creating network segment from config",
    );
    let segment = crate::handlers::network_segment::save_without_reverse_zones(
        api,
        txn,
        ns,
        true,
        allocate_svi_ip,
        allocations,
    )
    .await?;
    db::network_segment::insert_network_def(txn, name, segment_id, def).await?;
    Ok(segment)
}

async fn repair_initial_network_dns(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    networks: &HashMap<String, NetworkDefinition>,
    domain_id: DomainId,
) -> Result<(), CarbideError> {
    ensure_static_assignments_segment(api, txn, Some(domain_id)).await?;
    let network_definition_names = networks.keys().cloned().collect_vec();
    let mut prefixes = db::network_prefix::find_persisted_for_network_definitions(
        &mut *txn,
        &network_definition_names,
    )
    .await?;
    let static_assignments = db::network_segment::static_assignments(&mut *txn).await?;
    if !static_assignments.is_marked_as_deleted() {
        prefixes.extend(
            static_assignments
                .prefixes
                .iter()
                .map(|prefix| prefix.prefix),
        );
    }
    db::dns::ensure_reverse_zones(&prefixes, txn).await?;
    Ok(())
}

// A live seeder can use two bounded allocation preparations. A crashed seeder's
// name lock expires while shared recovery continues in the waiting process.
const INITIAL_NETWORK_WAIT_TIMEOUT: Duration = Duration::from_secs(90);
const INITIAL_NETWORK_LOCK_RETRY: Duration = Duration::from_millis(100);
const INITIAL_NETWORK_CLEANUP_RETRY: Duration = Duration::from_secs(1);

async fn lock_initial_network(api: &Api, name: &str) -> Result<WorkLock, CarbideError> {
    let work_key = format!("initial-network-segment:{name}");
    let acquire = async {
        loop {
            match api
                .work_lock_manager_handle
                .try_acquire_lock(work_key.clone())
                .await
            {
                Ok(lock) => return Ok(lock),
                Err(AcquireLockError::WorkAlreadyLocked(_)) => {
                    tokio::time::sleep(INITIAL_NETWORK_LOCK_RETRY).await
                }
                Err(error) => {
                    return Err(CarbideError::from(RemoteResourcePoolError::Acquire(error)));
                }
            }
        }
    };
    api.cancel_token
        .run_until_cancelled(tokio::time::timeout(INITIAL_NETWORK_WAIT_TIMEOUT, acquire))
        .await
        .ok_or_else(|| {
            CarbideError::from(ResourcePoolBackendError::Cancelled {
                operation: "configured network creation lock",
            })
        })?
        .map_err(|_| {
            CarbideError::from(ResourcePoolBackendError::DeadlineExceeded {
                operation: "configured network creation lock",
            })
        })?
}

async fn initial_network_phase<T>(
    api: &Api,
    phase: impl Future<Output = Result<T, CarbideError>>,
) -> Result<T, CarbideError> {
    initial_network_phase_until(
        api,
        tokio::time::Instant::now() + SEGMENT_COMPLETION_TIMEOUT,
        phase,
    )
    .await
}

async fn initial_network_phase_until<T>(
    api: &Api,
    deadline: tokio::time::Instant,
    phase: impl Future<Output = Result<T, CarbideError>>,
) -> Result<T, CarbideError> {
    api.cancel_token
        .run_until_cancelled(tokio::time::timeout_at(deadline, phase))
        .await
        .ok_or_else(|| {
            CarbideError::from(ResourcePoolBackendError::Cancelled {
                operation: "configured network creation transaction",
            })
        })?
        .map_err(|_| {
            CarbideError::from(ResourcePoolBackendError::DeadlineExceeded {
                operation: "configured network creation transaction",
            })
        })?
}

/// Start a transaction that may create this configured segment. If another
/// caller already created it, save any missing configuration snapshot here.
async fn transaction_for_missing_network(
    api: &Api,
    name: &str,
    def: &NetworkDefinition,
    name_lock: &WorkLock,
) -> Result<Option<db::Transaction<'static>>, CarbideError> {
    let mut txn = database_transaction(&api.database_connection).await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    name_lock.fence_transaction(&mut txn).await?;
    if db::network_segment::reconcile_network_def(&mut txn, name, def, &api.instrumentation).await?
    {
        Ok(Some(txn))
    } else {
        txn.commit().await?;
        Ok(None)
    }
}

async fn create_initial_network_individually(
    api: &Api,
    name: &str,
    def: &NetworkDefinition,
    domain_id: DomainId,
) -> Result<(), CarbideError> {
    if !SegmentAllocations::requires_external_preparation(api, def.segment_type.into()) {
        return create_initial_network_in_transaction(api, name, def, domain_id).await;
    }
    let name_lock = lock_initial_network(api, name).await?;
    let cleanup_deadline = tokio::time::Instant::now() + INITIAL_NETWORK_WAIT_TIMEOUT;
    loop {
        let ns = initial_network_phase(api, async {
            let Some(mut txn) = transaction_for_missing_network(api, name, def, &name_lock).await?
            else {
                return Ok(None);
            };
            let ns = NewNetworkSegment::build_from(name, domain_id, def)?;
            let (ns, _) = configure_segment_vpc(&mut txn, ns, def, VpcRowLock::None).await?;
            validate_segment_persistence(&mut txn, &ns).await?;
            txn.commit().await?;
            Ok(Some(ns))
        })
        .await?;
        let Some(ns) = ns else {
            return Ok(());
        };
        let allocations = match SegmentAllocations::prepare(api, &ns).await {
            Ok(allocations) => allocations,
            Err(failure) => {
                if api.cancel_token.is_cancelled() {
                    return Err(failure.into());
                }
                // API creation does not take the startup name lock. A winner
                // may have committed the last eligible value while we prepared.
                match initial_network_phase(
                    api,
                    transaction_for_missing_network(api, name, def, &name_lock),
                )
                .await
                {
                    Ok(None) => return Ok(()),
                    Ok(Some(_)) => {}
                    Err(recheck_error) => {
                        tracing::warn!(network_segment_name = name, error = %failure.error, %recheck_error,
                            "Could not recheck configured segment after allocation failed");
                        return Err(if api.cancel_token.is_cancelled() {
                            recheck_error
                        } else {
                            failure.into()
                        });
                    }
                }
                wait_for_initial_network_cleanup(
                    api,
                    name,
                    &ns.id.to_string(),
                    cleanup_deadline,
                    failure,
                )
                .await?;
                continue;
            }
        };
        let completion = allocations
            .complete(api, async {
                let Some(mut txn) =
                    transaction_for_missing_network(api, name, def, &name_lock).await?
                else {
                    return Ok(false);
                };
                // Other replicas can use this segment after commit, even if a later
                // declaration fails. Resolve its VPC and required FNN SVI together.
                let (ns, allocate_svi_ip) =
                    configure_segment_vpc(&mut txn, ns, def, VpcRowLock::Mutation).await?;
                persist_initial_network_with_dns(
                    api,
                    &mut txn,
                    name,
                    def,
                    ns,
                    &allocations,
                    allocate_svi_ip,
                )
                .await?;
                txn.commit().await?;
                Ok(true)
            })
            .await;
        if !matches!(completion, Ok(true)) {
            allocations.cancel(api).await;
        }
        if completion? {
            tracing::info!(network_segment_name = name, "Created network segment");
        }
        return Ok(());
    }
}

/// Entity-transaction segments have no expiring provider reservation. Retain
/// their ordinary SQL waits even at a site that also seeds external pools.
async fn create_initial_network_in_transaction(
    api: &Api,
    name: &str,
    def: &NetworkDefinition,
    domain_id: DomainId,
) -> Result<(), CarbideError> {
    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
    if db::network_segment::reconcile_network_def(&mut txn, name, def, &api.instrumentation).await?
    {
        let ns = NewNetworkSegment::build_from(name, domain_id, def)?;
        let (ns, allocate_svi_ip) =
            configure_segment_vpc(&mut txn, ns, def, VpcRowLock::None).await?;
        let allocations = SegmentAllocations::entity_transaction(&ns);
        persist_initial_network_with_dns(
            api,
            &mut txn,
            name,
            def,
            ns,
            &allocations,
            allocate_svi_ip,
        )
        .await?;
        txn.commit().await?;
        tracing::info!(network_segment_name = name, "Created network segment");
    } else {
        txn.commit().await?;
    }
    Ok(())
}

async fn persist_initial_network_with_dns(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    name: &str,
    def: &NetworkDefinition,
    ns: NewNetworkSegment,
    allocations: &SegmentAllocations<'_>,
    allocate_svi_ip: bool,
) -> Result<(), CarbideError> {
    // The reserved anchor must coexist with every independently committed
    // declaration, including underlay prefixes.
    ensure_static_assignments_segment(api, txn, ns.subdomain_id).await?;
    let segment =
        persist_initial_network(api, txn, name, def, ns, allocations, allocate_svi_ip).await?;
    let prefixes = segment
        .prefixes
        .iter()
        .map(|prefix| prefix.prefix)
        .collect_vec();
    db::dns::ensure_reverse_zones(&prefixes, txn).await?;
    Ok(())
}

/// Wait only when recovery still owes cleanup in the pool that was exhausted.
/// A completed handoff may race exhaustion; a later startup can retry that case.
async fn wait_for_initial_network_cleanup(
    api: &Api,
    name: &str,
    current_owner: &str,
    cleanup_deadline: tokio::time::Instant,
    failure: SegmentPreparationError<'_>,
) -> Result<(), CarbideError> {
    let error = failure.error;
    let Some(pool) = failure.exhausted_pool else {
        return Err(error);
    };
    if tokio::time::Instant::now() >= cleanup_deadline {
        return Err(error);
    }
    let probe_deadline =
        cleanup_deadline.min(tokio::time::Instant::now() + SEGMENT_COMPLETION_TIMEOUT);
    let pending_cleanup = initial_network_phase_until(api, probe_deadline, async {
        let mut txn = database_transaction(&api.database_connection).await?;
        let pending = carbide_resource_pool::cleanup_may_free_capacity(
            &mut txn,
            pool,
            OwnerType::NetworkSegment,
            current_owner,
        )
        .await?;
        txn.commit().await?;
        Ok(pending)
    })
    .await;
    match pending_cleanup {
        Ok(true) => {}
        Ok(false) => return Err(error),
        Err(probe_error) => {
            tracing::warn!(network_segment_name = name, %error, %probe_error,
                "Could not check allocation cleanup after exhaustion");
            return Err(if api.cancel_token.is_cancelled() {
                probe_error
            } else {
                error
            });
        }
    }
    tracing::info!(network_segment_name = name, %error,
        "Waiting for earlier network allocation cleanup before retrying startup");
    let waited = api
        .cancel_token
        .run_until_cancelled(tokio::time::timeout_at(
            cleanup_deadline,
            tokio::time::sleep(INITIAL_NETWORK_CLEANUP_RETRY),
        ))
        .await
        .ok_or_else(|| {
            CarbideError::from(ResourcePoolBackendError::Cancelled {
                operation: "configured network allocation cleanup",
            })
        })?;
    if waited.is_err() || tokio::time::Instant::now() >= cleanup_deadline {
        return Err(error);
    }
    Ok(())
}

async fn configure_segment_vpc(
    txn: &mut db::Transaction<'_>,
    mut ns: NewNetworkSegment,
    def: &NetworkDefinition,
    row_lock: VpcRowLock,
) -> Result<(NewNetworkSegment, bool), CarbideError> {
    let name = &ns.name;
    ns.can_stretch = Some(true);
    let allocate_svi_ip = if let Some(vpc_name) = &def.vpc_name {
        match db::vpc::find_by_with_lock(
            txn.as_mut(),
            ObjectColumnFilter::One(db::vpc::NameColumn, &vpc_name.as_str()),
            row_lock,
        )
        .await?
        .as_slice()
        {
            [vpc] => {
                let virtualization_type = vpc.config.network_virtualization_type;
                virtualization_type.ensure_supports_segment(&ns)?;
                ns.vpc_id = Some(vpc.id);
                virtualization_type.allocates_svi_for(&ns)
            }
            [] => {
                return Err(CarbideError::InvalidArgument(format!(
                    "network segment {name} references VPC {vpc_name}, but no VPC with that name exists"
                )));
            }
            _ => {
                return Err(CarbideError::InvalidArgument(format!(
                    "network segment {name} references VPC {vpc_name}, but multiple VPCs with that name exist"
                )));
            }
        }
    } else {
        false
    };

    Ok((ns, allocate_svi_ip))
}

pub(crate) fn validate_initial_vpcs(
    vpcs: &HashMap<String, VpcDefinition>,
) -> Result<(), model::ConfigValidationError> {
    for (name, definition) in vpcs {
        // Inline overrides are supported only by runtime VPC creation requests.
        if definition.routing_profile_overrides.is_some() {
            return Err(
                model::ConfigValidationError::InitialVpcRoutingProfileOverridesUnsupported {
                    name: name.clone(),
                },
            );
        }
    }
    Ok(())
}

/// Creates each configured VPC independently and skips names already present.
/// Earlier commits survive a later failure, so startup can resume next time.
pub(crate) async fn create_initial_vpcs(
    api: &Api,
    vpcs: &HashMap<String, VpcDefinition>,
) -> Result<(), CarbideError> {
    // Validate here as well as during SeedData resolution, before any
    // existing-name shortcut or creation through this entry point.
    validate_initial_vpcs(vpcs).map_err(CarbideError::InvalidConfiguration)?;

    for (name, def) in vpcs.iter().sorted_by_key(|(name, _)| *name) {
        if !db::vpc::find_by_name(&api.database_connection, name)
            .await?
            .is_empty()
        {
            tracing::debug!(vpc_name = %name, "VPC exists");
            continue;
        }
        if def.routing_profile_type.is_some() || def.routing_profile_overrides.is_some() {
            def.network_virtualization_type
                .ensure_supports_routing_profiles()
                .map_err(CarbideError::from)?;
        }
        let new_vpc = NewVpc {
            id: VpcId::new(),
            tenant_organization_id: def
                .organization_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().into()),
            network_virtualization_type: def.network_virtualization_type,
            metadata: Metadata {
                name: name.to_owned(),
                ..Default::default()
            },
            network_security_group_id: None,
            dhcp_option_set_id: None,
            routing_profile_type: def.routing_profile_type.clone(),
            routing_profile_overrides: def.routing_profile_overrides.clone(),
            power_resource_group: None,
            vni: def.vni,
            slaac_enabled: false,
        };
        if crate::handlers::vpc::create_configured(api, new_vpc).await? {
            tracing::info!(vpc_name = %name, "Created VPC");
        }
    }
    Ok(())
}

/// Create the static-assignments anchor segment if it doesn't exist.
/// This segment holds external static IP assignments that don't fall
/// within any managed network prefix. The placeholder prefixes are never
/// handed out by the allocator; they exist because the schema requires
/// segment prefixes and because static assignments can be IPv4 or IPv6.
pub(crate) async fn ensure_static_assignments_segment(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    subdomain_id: Option<carbide_uuid::domain::DomainId>,
) -> Result<(), CarbideError> {
    let segment_name = network_segment::STATIC_ASSIGNMENTS_SEGMENT_NAME;
    match db::network_segment::find_by_name(&mut *txn, segment_name).await {
        Ok(_) => return Ok(()),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error.into()),
    }

    let ns = NewNetworkSegment {
        id: uuid::Uuid::new_v4().into(),
        name: segment_name.to_string(),
        subdomain_id,
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![
            NewNetworkPrefix {
                prefix: STATIC_ASSIGNMENTS_IPV4_PREFIX.parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
            NewNetworkPrefix {
                prefix: STATIC_ASSIGNMENTS_IPV6_PREFIX.parse().unwrap(),
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 1,
            },
        ],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Underlay,
        can_stretch: Some(false),
        allocation_strategy: model::network_segment::AllocationStrategy::Reserved,
        infer_slaac_eui64_addresses: false,
    };
    crate::handlers::network_segment::save_without_reverse_zones(
        api,
        txn,
        ns,
        true,
        false,
        &crate::handlers::network_segment::SegmentAllocations::underlay(),
    )
    .await?;
    tracing::info!(
        network_segment_name = segment_name,
        "Created internal segment for holding static assignments",
    );

    Ok(())
}

pub(crate) async fn update_network_segments_svi_ip(
    db_pool: &Pool<Postgres>,
) -> Result<(), CarbideError> {
    let all_segments = db::network_segment::find_by(
        &mut PgPoolReader::from(db_pool.clone()),
        ObjectColumnFilter::<network_segment::IdColumn>::All,
        model::network_segment::NetworkSegmentSearchConfig::default(),
    )
    .await?;

    let all_segments = all_segments
        .into_iter()
        .filter(|x| x.status.can_stretch.is_some_and(|x| x))
        .collect::<Vec<_>>();

    let all_vpcs_ids = all_segments
        .iter()
        .filter_map(|x| x.config.vpc_id)
        .collect_vec();
    let all_vpcs = db::vpc::find_by(
        db_pool,
        ObjectColumnFilter::List(vpc::IdColumn, &all_vpcs_ids),
    )
    .await?;

    let all_vpcs = all_vpcs
        .iter()
        .map(|x| (x.id, x))
        .collect::<HashMap<_, _>>();

    // Allocate SVI IP for the segments attached to a FNN VPC.
    for segment in all_segments {
        let Some(vpc_id) = segment.config.vpc_id else {
            continue;
        };

        let Some(vpc) = all_vpcs.get(&vpc_id) else {
            continue;
        };

        // SVI IP is needed only for FNN.
        if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
            continue;
        }

        // Already SVI IP is allocated for every prefix. Prefixless segments
        // still fall through so allocate_svi_ip reports the invalid state.
        if !segment.prefixes.is_empty() && segment.prefixes.iter().all(|x| x.svi_ip.is_some()) {
            continue;
        }

        let mut txn = db_pool.txn_begin().await?;

        match db::network_segment::allocate_svi_ip(&segment, &mut txn).await {
            Ok(_) => {
                txn.commit().await?;
            }
            Err(err) => {
                tracing::error!(
                    network_segment_id = %segment.id,
                    error = %err,
                    "Failed to update SVI IP",
                );
                txn.rollback().await?;
            }
        }
    }

    Ok(())
}

pub(crate) async fn store_initial_dpu_agent_upgrade_policy(
    db_pool: &Pool<Postgres>,
    initial_dpu_agent_upgrade_policy: Option<AgentUpgradePolicyChoice>,
) -> Result<(), CarbideError> {
    let mut txn = db_pool.txn_begin().await?;
    let initial_policy: AgentUpgradePolicy = initial_dpu_agent_upgrade_policy
        .unwrap_or(AgentUpgradePolicyChoice::UpDown)
        .into();
    let current_policy = dpu_agent_upgrade_policy::get(&mut txn).await?;
    // Only set if the very first time, it's the initial policy
    if current_policy.is_none() {
        dpu_agent_upgrade_policy::set(&mut txn, initial_policy).await?;
        tracing::debug!(
            %initial_policy,
            "Initialized DPU agent upgrade policy"
        );
    }
    txn.commit().await?;

    Ok(())
}

pub(crate) async fn create_admin_vpc(api: &Api, vpc_vni: Option<u32>) -> Result<(), CarbideError> {
    let Some(vpc_vni) = vpc_vni else {
        return Err(CarbideError::internal(
            "no VNI is configured for admin VPC".to_string(),
        ));
    };

    // Resolve the policy with the VNI reconciliation will store before changing VPCs or segments.
    if let Some(admin) = api
        .runtime_config
        .fnn
        .as_ref()
        .and_then(|fnn| fnn.admin_vpc.as_ref())
        .filter(|admin| admin.enabled)
    {
        admin
            .routing_profile
            .to_rpc(Some(vpc_vni))
            .map_err(|error| CarbideError::FailedPrecondition(error.to_string()))?;
    }

    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(&mut txn).await?;

    let configured_vni = vpc_vni as i32;
    let admin_segments = db::network_segment::admin(&mut txn).await?;
    let attached_admin_vpc_ids = admin_segments
        .iter()
        .filter_map(|segment| segment.config.vpc_id)
        .unique()
        .collect_vec();

    // Admin VPC startup reconciliation has three expected cases:
    // 1. Fresh install with admin FNN enabled: no admin segments are attached
    //    and no VPC should already own the configured admin VNI, so create it.
    // 2. Existing install with admin FNN already seeded: admin segments point
    //    at one admin VPC, so that attached VPC is authoritative and its VNI
    //    may be updated from config.
    // 3. Existing install enabling admin FNN for the first time: admin segments
    //    are unattached, but tenant VPCs may already exist. In this case we
    //    must reject if any VPC already owns the configured admin VNI rather
    //    than adopting that VPC as the admin VPC.
    let existing_vpc = match attached_admin_vpc_ids.as_slice() {
        [admin_vpc_id] => {
            // The attached VPC is the authoritative admin VPC across config changes.
            let mut vpcs = db::vpc::find_by(
                &mut txn,
                ObjectColumnFilter::One(vpc::IdColumn, admin_vpc_id),
            )
            .await?;
            if vpcs.len() != 1 {
                return Err(CarbideError::internal(format!(
                    "admin network segment references missing VPC {admin_vpc_id}"
                )));
            }
            Some(vpcs.remove(0))
        }
        [] => {
            // This is first-time admin VPC seeding. Do not adopt an existing
            // tenant VPC that happens to use the configured admin VNI.
            if let Some(conflicting_vpc) =
                db::vpc::find_by_vni(&mut txn, configured_vni).await?.pop()
            {
                return Err(CarbideError::internal(format!(
                    "configured admin VPC VNI {configured_vni} is already used by VPC {}, but no admin VPC is attached to admin network segments",
                    conflicting_vpc.id
                )));
            }
            None
        }
        _ => {
            return Err(CarbideError::internal(format!(
                "admin network segments are attached to multiple VPCs: {}",
                attached_admin_vpc_ids.iter().join(", ")
            )));
        }
    };

    if let Some(mut existing_vpc) = existing_vpc {
        let existing_vni = existing_vpc.status.vni;
        if existing_vni != Some(configured_vni) || existing_vpc.config.vni != Some(configured_vni) {
            if let Some(conflicting_vpc) = db::vpc::find_by_vni(&mut txn, configured_vni)
                .await?
                .into_iter()
                .find(|vpc| vpc.id != existing_vpc.id)
            {
                return Err(CarbideError::internal(format!(
                    "configured admin VPC VNI {configured_vni} is already used by VPC {}, but admin network segments are attached to VPC {}",
                    conflicting_vpc.id, existing_vpc.id
                )));
            }

            existing_vpc = db::vpc::set_vni(&existing_vpc, &mut txn, configured_vni).await?;
            tracing::info!(
                vpc_id = %existing_vpc.id,
                previous_vni = ?existing_vni,
                configured_vni,
                "Updated admin VPC VNI from FNN config"
            );
        }

        for admin_segment in admin_segments {
            match admin_segment.config.vpc_id {
                Some(vpc_id) if vpc_id != existing_vpc.id => {
                    return Err(CarbideError::internal(format!(
                        "mismatch found in admin vpc id {} and admin network segment's attached vpc id {vpc_id}",
                        existing_vpc.id
                    )));
                }
                Some(_) => {}
                None => {
                    // Attach any newly-created admin segment to the existing admin VPC.
                    db::network_segment::set_vpc_id_and_can_stretch(
                        &admin_segment,
                        &mut txn,
                        existing_vpc.id,
                    )
                    .await?;
                }
            }
        }

        crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(
            api, &mut txn,
        )
        .await?;
        txn.commit().await?;

        return Ok(());
    }

    // Let's create admin vpc.
    let admin_vpc = NewVpc {
        id: uuid::Uuid::new_v4().into(),
        vni: Some(configured_vni),
        tenant_organization_id: "carbide_internal".to_string(),
        // For consistency, but admin routing profile is defined in-line in the
        // FNN config.
        routing_profile_type: None, // It's purely informational.  Admin profile is pulled from an inline-config and not tied to a name or ID.
        routing_profile_overrides: None,
        power_resource_group: None,
        slaac_enabled: false,
        network_security_group_id: None,
        dhcp_option_set_id: None,
        network_virtualization_type: carbide_network::virtualization::VpcVirtualizationType::Fnn,
        metadata: Metadata {
            name: "admin".to_string(),
            labels: HashMap::from([("kind".to_string(), "admin".to_string())]),
            ..Metadata::default()
        },
    };

    let vpc = db::vpc::persist(
        admin_vpc,
        VpcStatus {
            vni: Some(configured_vni),
        },
        &mut txn,
    )
    .await?;

    // Attach it to admin network segments.
    for admin_segment in admin_segments {
        db::network_segment::set_vpc_id_and_can_stretch(&admin_segment, &mut txn, vpc.id).await?;
    }

    crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(api, &mut txn)
        .await?;
    txn.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use chrono::Utc;

    use super::*;

    struct DomainSelectionCase {
        domains: Vec<Domain>,
        configured_domain_name: Option<&'static str>,
    }

    fn domain_id(id: u128) -> DomainId {
        uuid::Uuid::from_u128(id).into()
    }

    fn domain(id: u128, name: &str) -> Domain {
        let timestamp = Utc::now();
        Domain {
            id: domain_id(id),
            name: name.to_string(),
            default_ttl: None,
            created: timestamp,
            updated: timestamp,
            deleted: None,
            soa: None,
            metadata: None,
        }
    }

    #[test]
    fn initial_network_domain_selection_distinguishes_forward_and_reverse_domains() {
        // Reverse zones share the `domains` table, but only a forward domain
        // may become the parent for configured initial network segments.
        check_values(
            [
                Check {
                    scenario: "no domains are available",
                    input: DomainSelectionCase {
                        domains: vec![],
                        configured_domain_name: None,
                    },
                    expect: InitialNetworkDomainSelection::NoForwardDomain,
                },
                Check {
                    scenario: "the configured domain wins among multiple forward domains",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "legacy.example"),
                            domain(2, "0.20.172.IN-ADDR.ARPA."),
                            domain(3, "site.example"),
                        ],
                        configured_domain_name: Some("site.example"),
                    },
                    expect: InitialNetworkDomainSelection::Selected(domain_id(3)),
                },
                Check {
                    scenario: "a renamed configured domain falls back to the sole forward domain",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "renamed.example"),
                            domain(2, "0.20.172.in-addr.arpa"),
                        ],
                        configured_domain_name: Some("site.example"),
                    },
                    expect: InitialNetworkDomainSelection::Selected(domain_id(1)),
                },
                Check {
                    scenario: "an API-created domain works without initial_domain_name",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "api-created.example"),
                            domain(2, "0.20.172.in-addr.arpa"),
                            domain(3, "0.0.ip6.arpa"),
                        ],
                        configured_domain_name: None,
                    },
                    expect: InitialNetworkDomainSelection::Selected(domain_id(1)),
                },
                Check {
                    scenario: "reverse domains alone do not become the forward domain",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "0.20.172.in-addr.arpa"),
                            domain(2, "0.0.ip6.arpa."),
                        ],
                        configured_domain_name: None,
                    },
                    expect: InitialNetworkDomainSelection::NoForwardDomain,
                },
                Check {
                    scenario: "a configured reverse domain does not override the forward domain",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "site.example"),
                            domain(2, "0.20.172.in-addr.arpa"),
                        ],
                        configured_domain_name: Some("0.20.172.in-addr.arpa"),
                    },
                    expect: InitialNetworkDomainSelection::Selected(domain_id(1)),
                },
                Check {
                    scenario: "multiple forward domains without a configured match are ambiguous",
                    input: DomainSelectionCase {
                        domains: vec![
                            domain(1, "one.example"),
                            domain(2, "two.example"),
                            domain(3, "0.20.172.in-addr.arpa"),
                        ],
                        configured_domain_name: None,
                    },
                    expect: InitialNetworkDomainSelection::Ambiguous(vec![
                        "one.example".to_string(),
                        "two.example".to_string(),
                    ]),
                },
                Check {
                    scenario: "duplicate configured domains are ambiguous",
                    input: DomainSelectionCase {
                        domains: vec![domain(1, "site.example"), domain(2, "site.example")],
                        configured_domain_name: Some("site.example"),
                    },
                    expect: InitialNetworkDomainSelection::Ambiguous(vec![
                        "site.example".to_string(),
                        "site.example".to_string(),
                    ]),
                },
            ],
            |DomainSelectionCase {
                 domains,
                 configured_domain_name,
             }| { select_initial_network_domain(&domains, configured_domain_name) },
        );
    }
}

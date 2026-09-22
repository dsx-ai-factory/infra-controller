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

use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::domain::DomainId;
use carbide_uuid::vpc::VpcId;
use db::dns::domain;
use db::network_segment::reconcile_network_defs;
use db::vpc::{self};
use db::{ObjectColumnFilter, Transaction, dpu_agent_upgrade_policy, network_segment};
use itertools::Itertools;
use model::dns::{Domain, NewDomain};
use model::firmware::AgentUpgradePolicyChoice;
use model::machine::upgrade_policy::AgentUpgradePolicy;
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{NetworkDefinition, NetworkSegmentType, NewNetworkSegment};
use model::resource_pool;
use model::resource_pool::ResourcePool;
use model::vpc::{NewVpc, VpcDefinition, VpcStatus, VpcVirtualizationTypeCapabilities};
use sqlx::{Pool, Postgres};

use crate::CarbideError;
use crate::api::Api;

const STATIC_ASSIGNMENTS_IPV4_PREFIX: &str = "169.254.254.254/32";
const STATIC_ASSIGNMENTS_IPV6_PREFIX: &str = "100::/128";

/// Create a Domain if we don't already have one.
/// Returns true if we created an entry in the db (we had no domains yet), false otherwise.
pub(crate) async fn create_initial_domain(
    db_pool: sqlx::pool::Pool<Postgres>,
    domain_name: &str,
) -> Result<bool, CarbideError> {
    let mut txn = Transaction::begin(&db_pool).await?;
    let domains = domain::find_by(&mut txn, ObjectColumnFilter::<domain::IdColumn>::All).await?;
    if domains.is_empty() {
        let domain = NewDomain::new(domain_name);
        db::dns::domain::persist_first(&domain, &mut txn).await?;
        txn.commit().await?;
        Ok(true)
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
    let mut txn = Transaction::begin(db_pool).await?;
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;
    let domains = db::dns::domain::find_by(
        &mut txn,
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
    let to_create = reconcile_network_defs(&mut txn, networks).await?;

    for (name, def) in &to_create {
        let mut ns = NewNetworkSegment::build_from(name, domain_id, def)?;
        ns.can_stretch = Some(true);
        ns.vpc_id = if let Some(vpc_name) = &def.vpc_name {
            match db::vpc::find_by_name(&mut txn, vpc_name).await?.as_slice() {
                [vpc] => {
                    vpc.config
                        .network_virtualization_type
                        .ensure_supports_segment(&ns)?;
                    Some(vpc.id)
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
            None
        };

        // Capture before `save_without_reverse_zones` moves `ns`.
        // `insert_network_def` needs the id because
        // `network_def.segment_id` is FK-bound to it.
        let segment_id = ns.id;
        // update_network_segments_svi_ip will take care of allocating svi ip.
        tracing::info!(
            network_segment_name = %name,
            network_segment = ?ns,
            "Creating network segment from config",
        );
        crate::handlers::network_segment::save_without_reverse_zones(
            api, &mut txn, ns, true, false,
        )
        .await?;
        // Snapshot the network definition in the same transaction as the network_segment row,
        // so the two stay consistent across restarts.
        db::network_segment::insert_network_def(&mut txn, name, segment_id, def).await?;
        tracing::info!(
            network_segment_name = %name,
            "Created network segment",
        );
    }

    ensure_static_assignments_segment(api, &mut txn, Some(domain_id)).await?;
    let network_definition_names = networks.keys().cloned().collect_vec();
    let mut reverse_zone_prefixes = db::network_prefix::find_persisted_for_network_definitions(
        &mut txn,
        &network_definition_names,
    )
    .await?;
    let static_assignments = db::network_segment::static_assignments(&mut txn).await?;
    if !static_assignments.is_marked_as_deleted() {
        reverse_zone_prefixes.extend(
            static_assignments
                .prefixes
                .iter()
                .map(|prefix| prefix.prefix),
        );
    }
    db::dns::ensure_reverse_zones(&reverse_zone_prefixes, &mut txn).await?;

    crate::handlers::tenant_prefix_overlap::validate_retained_state_in_transaction(api, &mut txn)
        .await?;
    txn.commit().await?;
    Ok(())
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

pub(crate) async fn create_initial_vpcs(
    db_pool: &Pool<Postgres>,
    vpcs: &HashMap<String, VpcDefinition>,
    vni_pool: &ResourcePool<i32>,
) -> Result<(), CarbideError> {
    // Retain validation at the mutation boundary as defense in depth. Startup
    // also validates during SeedData resolution, before any reconciliation.
    validate_initial_vpcs(vpcs).map_err(CarbideError::InvalidConfiguration)?;

    let mut txn = Transaction::begin(db_pool).await?;
    for (name, def) in vpcs {
        if db::vpc::find_by_name(&mut txn, name)
            .await
            .is_ok_and(|v| !v.is_empty())
        {
            tracing::debug!(
                vpc_name = %name,
                "VPC exists",
            );
            continue;
        }

        let vpc_id = VpcId::new();
        let tenant_organization_id = def
            .organization_id
            .clone()
            .unwrap_or(uuid::Uuid::new_v4().into());
        let owner_id = vpc_id.to_string();

        let vni = db::resource_pool::allocate(
            vni_pool,
            &mut txn,
            resource_pool::OwnerType::Vpc,
            &owner_id,
            def.vni,
        )
        .await
        .inspect_err(|error| {
            db::resource_pool::emit_allocation_failure(
                vni_pool.value_type,
                &owner_id,
                def.vni.is_some(),
                vni_pool.name(),
                error,
            );
        })?;

        let vpc = NewVpc {
            id: vpc_id,
            tenant_organization_id,
            network_virtualization_type: def.network_virtualization_type,
            metadata: Metadata {
                name: name.to_owned(),
                ..Default::default()
            },
            network_security_group_id: None,
            routing_profile_type: def.routing_profile_type.clone(),
            routing_profile_overrides: def.routing_profile_overrides.clone(),
            power_resource_group: None,
            vni: Some(vni),
            slaac_enabled: false,
        };

        // Validation
        if def.routing_profile_type.is_some() || def.routing_profile_overrides.is_some() {
            def.network_virtualization_type
                .ensure_supports_routing_profiles()
                .map_err(CarbideError::from)?;
        }

        db::vpc::persist(vpc, VpcStatus { vni: Some(vni) }, &mut txn).await?;
        tracing::info!(
            vpc_name = %name,
            "Created VPC",
        );
    }

    txn.commit().await?;
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
    if db::network_segment::find_by_name(txn, segment_name)
        .await
        .is_ok()
    {
        return Ok(());
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
    crate::handlers::network_segment::save_without_reverse_zones(api, txn, ns, true, false).await?;
    tracing::info!(
        network_segment_name = segment_name,
        "Created internal segment for holding static assignments",
    );

    Ok(())
}

pub(crate) async fn update_network_segments_svi_ip(
    db_pool: &Pool<Postgres>,
) -> Result<(), CarbideError> {
    let mut txn = Transaction::begin(db_pool).await?;
    let all_segments = db::network_segment::find_by(
        &mut txn,
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
        &mut txn,
        ObjectColumnFilter::List(vpc::IdColumn, &all_vpcs_ids),
    )
    .await?;

    let all_vpcs = all_vpcs
        .iter()
        .map(|x| (x.id, x))
        .collect::<HashMap<_, _>>();

    txn.rollback().await?;

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

        let mut txn = Transaction::begin(db_pool).await?;

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
    let mut txn = Transaction::begin(db_pool).await?;
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

    let mut txn = api.txn_begin().await?;
    db::tenant_prefix_overlap::lock_checks(txn.as_mut()).await?;

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

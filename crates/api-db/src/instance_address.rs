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
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
#[cfg(test)]
use std::ops::DerefMut;

use carbide_network::virtualization::{VpcVirtualizationType, get_host_ip};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineIdSubtypeTrait;
use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use itertools::Itertools;
use model::ConfigValidationError;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::instance::config::network::{InstanceInterfaceConfig, InstanceNetworkConfig};
use model::instance_address::InstanceAddress;
use model::machine::{HostMachine, Machine};
use model::network_prefix::NetworkPrefix;
use model::network_segment::{
    NetworkSegment, NetworkSegmentControllerState, NetworkSegmentSearchConfig, NetworkSegmentType,
};
use sqlx::{FromRow, PgConnection, PgTransaction, query_as};

use super::{ObjectColumnFilter, network_segment, vpc};
use crate::db_read::DbReader;
use crate::instance::push_network_segment_reference_exists;
use crate::ip_allocator::{IpAllocator, UsedIpResolver};
use crate::{BIND_LIMIT, DatabaseError, DatabaseResult, Transaction};

/// Parameters bound per row by [`insert_instance_addresses`]; with one
/// statement holding at most [`BIND_LIMIT`] bindings, rows are written in
/// chunks of `BIND_LIMIT / ADDRESS_BINDS_PER_ROW`. Practical allocations
/// (interfaces x prefixes) sit far below one chunk, so the INSERT is a
/// single statement.
const ADDRESS_BINDS_PER_ROW: usize = 6;

/// Serializes operations that select or reserve tenant addresses.
///
/// Call this before locking any row in `network_prefixes`. Segment deletion
/// takes an ACCESS SHARE lock on this table before deleting prefix rows, so
/// keeping this lock order prevents allocation/deletion deadlocks.
pub(crate) async fn lock_table_for_allocation(txn: &mut PgConnection) -> Result<(), DatabaseError> {
    let query = "LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE";
    sqlx::query(query)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// `find_all_by_address` returns every overlay allocation using `address`.
/// An address is only unique inside its VPC, so callers without that context
/// must handle every row rather than letting PostgreSQL pick one.
pub async fn find_all_by_address(
    txn: impl DbReader<'_>,
    address: IpAddr,
) -> Result<Vec<InstanceAddress>, DatabaseError> {
    let query = "SELECT * FROM instance_addresses
        WHERE address = $1::inet
        ORDER BY vpc_id, segment_id, instance_id";
    sqlx::query_as(query)
        .bind(address)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Returns every address an instance holds on one segment, ordered by address.
/// A dual-stack segment has one row per address family, so this lookup is not
/// a zero-or-one relationship.
pub async fn find_all_by_instance_id_and_segment_id(
    txn: &mut PgConnection,
    instance_id: &InstanceId,
    segment_id: &NetworkSegmentId,
) -> Result<Vec<InstanceAddress>, DatabaseError> {
    let query = "SELECT * FROM instance_addresses
        WHERE instance_id=$1 AND segment_id=$2
        ORDER BY address";

    sqlx::query_as(query)
        .bind(instance_id)
        .bind(segment_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_by_segment_id(
    txn: impl DbReader<'_>,
    segment_id: &NetworkSegmentId,
) -> Result<Vec<InstanceAddress>, DatabaseError> {
    let query = "SELECT * FROM instance_addresses WHERE segment_id = $1::uuid ORDER BY address";
    sqlx::query_as(query)
        .bind(segment_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Locks every address row for an instance in stable ID order.
///
/// Both deletion paths call this before choosing rows to delete. Full instance
/// deletion consequently retains the existing address-before-instance order.
/// Allocation holds an `ACCESS EXCLUSIVE` table lock before inserting, so no
/// new address row can appear between this prelock and the following delete.
async fn lock_addresses_for_instance(
    txn: &mut PgConnection,
    instance_id: InstanceId,
) -> Result<(), DatabaseError> {
    let query = "SELECT id FROM instance_addresses
        WHERE instance_id=$1
        ORDER BY id
        FOR UPDATE";
    sqlx::query(query)
        .bind(instance_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Deletes every address allocated to an instance.
///
/// All address rows for the instance are locked in stable ID order before the
/// delete so this operation can safely overlap partial address cleanup.
pub async fn delete(txn: &mut PgConnection, instance_id: InstanceId) -> Result<(), DatabaseError> {
    lock_addresses_for_instance(txn, instance_id).await?;

    let query = "DELETE FROM instance_addresses WHERE instance_id=$1";
    sqlx::query(query)
        .bind(instance_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// `delete_addresses_for_instance` releases exact segment/address pairs from
/// one instance. The segment is part of the identity because an FNN instance
/// can use more than one VPC, including VPCs with equal address space.
/// Missing rows are ignored so a retried release remains idempotent.
pub async fn delete_addresses_for_instance(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    addresses: &[(NetworkSegmentId, IpAddr)],
) -> Result<(), DatabaseError> {
    if addresses.is_empty() {
        return Ok(());
    }

    lock_addresses_for_instance(txn, instance_id).await?;

    let segment_ids = addresses
        .iter()
        .map(|(segment_id, _)| *segment_id)
        .collect::<Vec<_>>();
    let released_addresses = addresses
        .iter()
        .map(|(_, address)| *address)
        .collect::<Vec<_>>();
    let query = "DELETE FROM instance_addresses stored
        USING UNNEST($2::uuid[], $3::inet[]) AS released(segment_id, address)
        WHERE stored.instance_id = $1
          AND stored.segment_id = released.segment_id
          AND stored.address = released.address";
    sqlx::query(query)
        .bind(instance_id)
        .bind(segment_ids)
        .bind(released_addresses)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

fn interface_vpc_id(iface: &InstanceInterfaceConfig, segments: &[NetworkSegment]) -> Option<VpcId> {
    iface.vpc_id.or_else(|| {
        let segment_id = iface.network_segment_id?;
        segments
            .iter()
            .find(|segment| segment.id == segment_id)
            .and_then(|segment| segment.config.vpc_id)
    })
}

fn validate(
    segments: &[NetworkSegment],
    instance_network: &InstanceNetworkConfig,
    segment_ids_using_vpc_prefix: &[NetworkSegmentId],
    all_fnn: bool,
) -> DatabaseResult<()> {
    if segments.len() != instance_network.interfaces.len() {
        // Missing at least one segment in db.
        return Err(ConfigValidationError::UnknownSegments.into());
    }

    let mut vpc_ids = HashSet::new();

    for segment in segments {
        if segment.is_marked_as_deleted() {
            // TODO: Single error for not ready and deleted?
            return Err(ConfigValidationError::NetworkSegmentToBeDeleted(segment.id).into());
        }

        // If segment is created using vpc_prefix id, it will not be in Ready state by now.
        if !segment_ids_using_vpc_prefix.contains(&segment.id) {
            match &segment.status.controller_state.value {
                NetworkSegmentControllerState::Ready => {}
                _ => {
                    return Err(ConfigValidationError::NetworkSegmentNotReady(
                        segment.id,
                        format!("{:?}", segment.status.controller_state.value),
                    )
                    .into());
                }
            }
        }
    }

    for iface in &instance_network.interfaces {
        match interface_vpc_id(iface, segments) {
            Some(vpc_id) => {
                vpc_ids.insert(vpc_id);
            }
            None => {
                let segment_id = iface
                    .network_segment_id
                    .ok_or(DatabaseError::NetworkSegmentNotAllocated)?;
                return Err(ConfigValidationError::VpcNotAttachedToSegment(segment_id).into());
            }
        }
    }

    if vpc_ids.len() != 1 && !all_fnn {
        return Err(ConfigValidationError::MultipleVpcFound.into());
    }

    Ok(())
}

/// Counts the amount of addresses that have been allocated for a given segment.
///
/// This diagnostic count intentionally covers only normalized
/// `instance_addresses` rows. [`segment_has_allocations`] is the broader
/// boolean used while draining a segment: it also checks machine interfaces
/// and current or pending instance network configurations.
pub async fn count_by_segment_id(
    txn: &mut PgConnection,
    segment_id: &NetworkSegmentId,
) -> Result<usize, DatabaseError> {
    // NOTE(chet): Previously this query used an INNER JOIN with
    // network_prefixes to count addresses per-prefix. For dual-stack
    // segments with multiple prefixes, the JOIN would double-count
    // addresses (once per prefix). The simplified query counts all
    // addresses for the segment directly, which works for both
    // single-prefix and multi-prefix segments.
    let query = "SELECT count(*) FROM instance_addresses WHERE segment_id = $1::uuid";
    let (address_count,): (i64,) = query_as(query)
        .bind(segment_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(address_count.max(0) as usize)
}

/// Returns whether a segment still holds any allocation — a machine
/// interface, an instance address, or a current/pending instance network
/// configuration bound to it.
///
/// The drain check only needs to know whether *any* allocation remains, so this
/// answers it in one round trip with three `EXISTS` arms. The arm that checks
/// instance configurations protects SLAAC allocations that retain only a
/// prefix; these deliberately have no row in `instance_addresses`.
/// Callers previously ran [`count_by_segment_id`] and
/// [`crate::machine_interface::count_by_segment_id`] and summed the totals —
/// two round trips to compute a boolean. Those count functions for each source
/// and [`crate::instance::count_network_segment_references`] remain in
/// production for `network_segment::mark_as_deleted` diagnostics. Their
/// individual scopes are narrower than this predicate; together they cover the
/// same three reference sources.
pub async fn segment_has_allocations(
    txn: &mut PgConnection,
    segment_id: &NetworkSegmentId,
) -> Result<bool, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT
            EXISTS(SELECT 1 FROM machine_interfaces WHERE segment_id = ",
    );
    builder.push_bind(*segment_id);
    builder.push(
        ")
            OR EXISTS(SELECT 1 FROM instance_addresses WHERE segment_id = ",
    );
    builder.push_bind(*segment_id);
    builder.push(
        ")
            OR EXISTS(
                SELECT 1
                FROM instances
                WHERE ",
    );
    push_network_segment_reference_exists(&mut builder, *segment_id);
    builder.push(")");

    builder
        .build_query_scalar()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Persists every [`InstanceAddress`] row `allocate` accumulated, with a
/// single multi-row INSERT -- collapsing the write side of the exclusive lock
/// window to one statement instead of one per address. Callers are expected
/// to hold the `instance_addresses` exclusive lock so the batched write
/// closes the allocation atomically.
///
/// Each row binds six parameters and one statement holds at most
/// [`BIND_LIMIT`] bindings, so rows are written in chunks of
/// `BIND_LIMIT / 6`. Practical row counts (interfaces × prefixes) are far
/// below one chunk, so this issues exactly one statement; only a degenerate,
/// huge allocation splits into multiple statements, keeping every row count
/// insertable.
async fn insert_instance_addresses(
    txn: &mut PgConnection,
    rows: &[InstanceAddress],
) -> DatabaseResult<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let query = "INSERT INTO instance_addresses \
        (instance_id, address, segment_id, prefix, vpc_id, hostname) ";
    for chunk in rows.chunks(BIND_LIMIT / ADDRESS_BINDS_PER_ROW) {
        let mut qb = sqlx::QueryBuilder::new(query);
        qb.push_values(chunk.iter(), |mut b, row| {
            b.push_bind(row.instance_id)
                .push_bind(row.address)
                .push_bind(row.segment_id)
                .push_bind(row.prefix)
                .push_bind(row.vpc_id)
                .push_bind(&row.hostname);
        });

        qb.build()
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }

    Ok(())
}

/// Tries to allocate IP addresses for a tenant network configuration
/// Returns the updated configuration which includes allocated addresses
pub async fn allocate(
    txn: &mut PgConnection,
    instance_id: InstanceId,
    mut updated_config: InstanceNetworkConfig,
    machine: &HostMachine,
) -> DatabaseResult<InstanceNetworkConfig> {
    // We expect only one prefix per segment (IPv4 or IPv6).
    // We're potentially about to insert a couple rows, so create a savepoint.
    let mut inner_txn = Transaction::begin_inner(txn).await?;

    let segment_ids = updated_config
        .interfaces
        .iter()
        .filter_map(|x| x.network_segment_id)
        .collect_vec();

    let segment_ids_using_vpc_prefix = updated_config
        .interfaces
        .iter()
        .filter_map(InstanceInterfaceConfig::generated_network_segment_id)
        .collect_vec();

    if segment_ids.len() != updated_config.interfaces.len() {
        return Err(DatabaseError::NetworkSegmentNotAllocated);
    }

    // Serialize allocation with segment/VPC deletion before reading either
    // parent. Deletion paths take ACCESS SHARE while checking allocations, so
    // whichever transaction acquires its table lock first determines whether
    // this allocation is retained or observes a deleted parent and fails.
    lock_table_for_allocation(inner_txn.as_pgconn()).await?;

    let segments = crate::network_segment::find_by(
        &mut inner_txn,
        ObjectColumnFilter::List(network_segment::IdColumn, &segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;

    // Load every referenced active VPC once. Besides validating multi-VPC FNN
    // use, the persisted VPC configuration selects address materialization
    // policy, so a missing row must fail rather than silently disabling SLAAC.
    let vpc_ids = updated_config
        .interfaces
        .iter()
        .filter_map(|iface| interface_vpc_id(iface, &segments))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect_vec();
    let vpcs = if vpc_ids.is_empty() {
        Vec::new()
    } else {
        vpc::find_by(
            &mut inner_txn,
            ObjectColumnFilter::List(vpc::IdColumn, &vpc_ids),
        )
        .await?
    };
    if vpcs.len() != vpc_ids.len() {
        return Err(DatabaseError::FailedPrecondition(format!(
            "instance network configuration references {} VPCs, but only {} active VPCs were found",
            vpc_ids.len(),
            vpcs.len(),
        )));
    }
    let all_fnn = if vpc_ids.len() > 1 {
        vpcs.iter()
            .all(|vpc| vpc.config.network_virtualization_type == VpcVirtualizationType::Fnn)
    } else {
        false
    };
    let slaac_vpc_ids = vpcs
        .iter()
        .filter(|vpc| vpc.config.slaac_enabled)
        .map(|vpc| vpc.id)
        .collect::<HashSet<_>>();

    validate(
        &segments,
        &updated_config,
        &segment_ids_using_vpc_prefix,
        all_fnn,
    )?;

    // Assign all addresses in one shot.
    //
    // Rows for every interface and every assigned address accumulate here and
    // are written with a single INSERT after the loops, so the write side of
    // the exclusive lock window is one statement rather than one per address.
    let mut rows: Vec<InstanceAddress> = Vec::new();

    for iface in &mut updated_config.interfaces {
        if !iface.ip_addrs.is_empty() {
            // IP is already allocated. Don't assign new IP.
            continue;
        }

        let segment = match segments
            .iter()
            .find(|x| iface.network_segment_id.map(|a| a == x.id).unwrap_or(false))
        {
            Some(x) => x,
            None => {
                if let Some(segment_id) = iface.network_segment_id {
                    return Err(DatabaseError::FindOneReturnedNoResultsError(
                        segment_id.into(),
                    ));
                }
                return Err(DatabaseError::NetworkSegmentNotAllocated);
            }
        };

        if segment.prefixes.is_empty() {
            tracing::error!(
                network_segment_id = %segment.id,
                "No prefix is attached to segment.",
            );
            return Err(DatabaseError::FindOneReturnedNoResultsError(
                segment.id.into(),
            ));
        }

        let vpc_id = interface_vpc_id(iface, &segments)
            .ok_or(ConfigValidationError::VpcNotAttachedToSegment(segment.id))?;
        if slaac_vpc_ids.contains(&vpc_id) {
            let requested_ipv6_address = iface
                .requested_ip_addr
                .filter(IpAddr::is_ipv6)
                .map(|address| address.to_string())
                .or_else(|| {
                    iface
                        .ipv6_interface_config
                        .as_ref()
                        .and_then(|config| config.requested_ip_addr)
                        .map(|address| address.to_string())
                });
            if let Some(requested_ip_addr) = requested_ipv6_address {
                return Err(DatabaseError::InvalidArgument(format!(
                    "requested IPv6 address `{requested_ip_addr}` is invalid because VPC `{vpc_id}` has SLAAC enabled",
                )));
            }
        }

        // Hydrate iface with network addresses, returning the assigned addresses.
        // A segment may have multiple prefixes (e.g. dual-stack with both IPv4 and IPv6).
        let addresses = if segment.config.segment_type == NetworkSegmentType::HostInband {
            // For host-inband network segments, the instance interface *is* the host
            // interface. Iterate all prefixes so dual-stack segments get both v4 and v6
            // addresses assigned. Prefixes where the host has no matching address are
            // skipped (e.g. a v6 prefix on a v4-only host).
            let mut all_addresses = Vec::new();
            for prefix in &segment.prefixes {
                match iface.assign_ips_from((machine, prefix)) {
                    Ok(mut assigned) => all_addresses.append(&mut assigned),
                    Err(DatabaseError::InvalidConfiguration(
                        ConfigValidationError::NetworkSegmentUnavailableOnHost,
                    )) => {
                        tracing::debug!(
                            network_segment_id = %segment.id,
                            prefix = %prefix.prefix,
                            "Host has no address in this prefix, skipping.",
                        );
                    }
                    Err(e) => return Err(e),
                }
            }
            if all_addresses.is_empty() {
                return Err(DatabaseError::InvalidConfiguration(
                    ConfigValidationError::NetworkSegmentUnavailableOnHost,
                ));
            }
            all_addresses
        } else {
            // Use the UsedOverlayNetworkIpResolver, which specifically looks at
            // the instance addresses table in the database for finding
            // the next available IP prefix allocation (with [assumed] support for
            // allocations of varying-sized networks).
            // Collect SVI IPs from all prefixes as reserved addresses.
            let busy_ips: Vec<IpAddr> = segment
                .prefixes
                .iter()
                .flat_map(|p| p.svi_ip.iter().copied())
                .collect();

            let dhcp_handler: Box<dyn UsedIpResolver<PgConnection> + Send> =
                Box::new(UsedOverlayNetworkIpResolver {
                    segment_id: segment.id,
                    busy_ips,
                });

            // TODO(chet): FNN will need to override prefix_length (e.g. /30
            // for IPv4, /126 for IPv6) via InstanceInterfaceConfig. For now,
            // the allocator defaults to single-host allocation (/32 or /128).
            let ip_allocator = IpAllocator::new(
                inner_txn.as_pgconn(),
                segment,
                dhcp_handler,
                AddressSelectionStrategy::NextAvailableIp,
            )
            .await?;

            let slaac_interface_prefixes = if slaac_vpc_ids.contains(&vpc_id) {
                segment
                    .prefixes
                    .iter()
                    .filter(|prefix| prefix.prefix.is_ipv6())
                    .map(|prefix| (prefix.id, prefix.prefix))
                    .collect()
            } else {
                HashMap::new()
            };

            iface.assign_ips_from(OverlayAddressAllocation {
                ip_allocator,
                slaac_interface_prefixes,
            })?
        };

        iface.vpc_id = Some(vpc_id);

        for address in addresses {
            let hostname = crate::host_naming::address_to_hostname(&address.ip())?;
            rows.push(InstanceAddress {
                instance_id,
                // eg. 10.3.2.1
                address: address.ip(),
                segment_id: segment.id,
                // eg. 10.3.2.0/30
                prefix: IpNetwork::new(address.network(), address.prefix())?,
                vpc_id,
                hostname: Some(hostname),
            });
        }
    }

    // Persist every accumulated address with one INSERT, still under the lock.
    insert_instance_addresses(inner_txn.as_pgconn(), &rows).await?;

    inner_txn.commit().await?;

    Ok(updated_config)
}

pub struct UsedOverlayNetworkIpResolver {
    pub segment_id: NetworkSegmentId,
    // All the IPs which can not be allocated, e.g. SVI IP.
    pub busy_ips: Vec<IpAddr>,
}

#[async_trait::async_trait]
impl<DB> UsedIpResolver<DB> for UsedOverlayNetworkIpResolver
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    // DEPRECATED
    // With the introduction of `used_prefixes()` this is no
    // longer an accurate approach for finding all allocated
    // IPs in a segment, since used_ips() completely ignores
    // the fact wider prefixes may have been allocated.
    //
    // used_ips returns the used (or allocated) IPs for instances
    // in a given network segment.
    //
    // More specifically, this is intended to specifically
    // target the `address` column of the `instance_addresses`
    // table, in which a single /32 is stored (although, as an
    // `inet`, it could techincally also have a prefix length).
    async fn used_ips(&self, txn: &mut DB) -> Result<Vec<IpAddr>, DatabaseError> {
        // IpAddrContainer is a small private struct used
        // for binding the result of the subsequent SQL
        // query, so we can implement FromRow and return
        // a Vec<IpAddr> a bit more easily.
        #[derive(FromRow)]
        struct IpAddrContainer {
            address: IpAddr,
        }

        let query: &str = "
SELECT address FROM instance_addresses
INNER JOIN network_segments ON instance_addresses.segment_id = network_segments.id
WHERE network_segments.id = $1::uuid";

        let containers: Vec<IpAddrContainer> = sqlx::query_as(query)
            .bind(self.segment_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        let mut used_ips: Vec<IpAddr> = containers.iter().map(|c| c.address).collect();
        used_ips.extend(self.busy_ips.iter());
        Ok(used_ips)
    }

    // used_prefixes returns the used (or allocated) prefixes
    // for instances in a given network segment.
    //
    // More specifically, this is intended to specifically
    // target the `prefix` column of the `instance_addresses`
    // table, which is a `cidr` type. It could contain as
    // small as a /32 (for single IP instance allocations,
    // which would effectively match the `address` column),
    // or a /30 (for FNN prefix allocations), where the `address`
    // column would contain the host IP allocated from the
    // /30 prefix.
    async fn used_prefixes(&self, txn: &mut DB) -> Result<Vec<IpNetwork>, DatabaseError> {
        // IpNetworkContainer is a small private struct used
        // for binding the result of the subsequent SQL
        // query, so we can implement FromRow and return
        // a Vec<IpNetwork> a bit more easily.
        #[derive(FromRow)]
        struct IpNetworkContainer {
            prefix: IpNetwork,
        }

        let query: &str = "
SELECT instance_addresses.prefix as prefix FROM instance_addresses
INNER JOIN network_segments ON instance_addresses.segment_id = network_segments.id
WHERE network_segments.id = $1::uuid";

        let containers: Vec<IpNetworkContainer> = sqlx::query_as(query)
            .bind(self.segment_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        Ok(containers.iter().map(|c| c.prefix).collect())
    }
}

/// Get IP addresses from Source, write them to self, and return them. Currently can come from an
/// IpAllocator, or from a host snapshot.
trait AssignIpsFrom<Source> {
    fn assign_ips_from(&mut self, source: Source) -> DatabaseResult<Vec<IpNetwork>>;
}

/// Overlay address policy resolved from the owning VPC.
///
/// IPv6 prefixes selected for SLAAC remain part of the durable interface
/// configuration, but do not produce a concrete host address. Other families
/// continue through the ordinary allocator.
struct OverlayAddressAllocation {
    ip_allocator: IpAllocator,
    slaac_interface_prefixes: HashMap<NetworkPrefixId, IpNetwork>,
}

impl<ID: MachineIdSubtypeTrait> AssignIpsFrom<(&Machine<ID>, &NetworkPrefix)>
    for InstanceInterfaceConfig
{
    // Zero-dpu config: For machines without DPUs, the machines's interface will be on an
    // HostInband network segment, which will be the same segment as the instance wants. In
    // this case, the host's interface *is* the instance interface, so we copy the config from it.
    fn assign_ips_from(
        &mut self,
        source: (&Machine<ID>, &NetworkPrefix),
    ) -> DatabaseResult<Vec<IpNetwork>> {
        let (machine, network_prefix) = source;

        // Find which interface on the machine is in this prefix
        let host_interfaces_in_instance_segment = machine
            .status
            .interfaces
            .iter()
            .filter(|i| {
                self.network_segment_id
                    .map(|a| a == i.segment_id)
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();

        if host_interfaces_in_instance_segment.len() > 1 {
            tracing::error!(
                "Managed host has multiple interfaces in the desired network segment. Cannot know which to assign to the instance config."
            );
            return Err(DatabaseError::FindOneReturnedManyResultsError(
                self.network_segment_id
                    .map(uuid::Uuid::from)
                    .unwrap_or_default(),
            ));
        }

        let Some(inband_host_interface) = host_interfaces_in_instance_segment.into_iter().next()
        else {
            return Err(DatabaseError::InvalidConfiguration(
                ConfigValidationError::NetworkSegmentUnavailableOnHost,
            ));
        };

        let matching_addresses = inband_host_interface
            .addresses
            .iter()
            .copied()
            .filter(|a| network_prefix.prefix.contains(*a))
            .collect::<Vec<_>>();

        if matching_addresses.len() > 1 {
            tracing::warn!(
                machine_id = %machine.id,
                prefix = %network_prefix.prefix,
                "Multiple IP addresses on managed host in the same network prefix, picking the first one to assign to instance"
            )
        }

        let Some(address) = matching_addresses.into_iter().next() else {
            return Err(DatabaseError::InvalidConfiguration(
                ConfigValidationError::NetworkSegmentUnavailableOnHost,
            ));
        };

        let assigned_address = IpNetwork::new(address, network_prefix.prefix.prefix())?;
        self.ip_addrs.insert(network_prefix.id, address);
        self.interface_prefixes
            .insert(network_prefix.id, network_prefix.prefix);

        self.host_inband_mac_address = Some(inband_host_interface.mac_address);

        // Also write out the gateway for the network segment's prefix. Unlike the interface_prefixes
        // field (which is a /32 or /30 for just this instance, for hosts with DPUs),
        // segment_gateway is the gateway for the entire network segment.
        //
        // This is currently only used for zero-DPU instances, where the instance's interface is
        // equivalent to the host's interface, and the tenant needs to know the gateway and prefix
        // to use for configuration.
        if let Some(prefix_gateway) = network_prefix.gateway {
            // gateway_as_network is the IP address of the gateway with the prefix length
            // appended. Example:
            // prefix_gateway: 192.168.1.1
            // network_prefix.prefix: 192.168.1.0/24
            // gateway_as_network: 192.168.1.1/24
            let gateway_as_network =
                IpNetwork::new(prefix_gateway, network_prefix.prefix.prefix())?;
            self.network_segment_gateways
                .insert(network_prefix.id, gateway_as_network);
        }

        Ok(vec![assigned_address])
    }
}

impl AssignIpsFrom<OverlayAddressAllocation> for InstanceInterfaceConfig {
    fn assign_ips_from(
        &mut self,
        source: OverlayAddressAllocation,
    ) -> DatabaseResult<Vec<IpNetwork>> {
        let OverlayAddressAllocation {
            ip_allocator,
            slaac_interface_prefixes,
        } = source;
        let mut addresses = Vec::new();
        for (prefix_id, allocated_prefix) in ip_allocator {
            if let Some(interface_prefix) = slaac_interface_prefixes.get(&prefix_id) {
                self.interface_prefixes.insert(prefix_id, *interface_prefix);
                continue;
            }

            let allocated_prefix = allocated_prefix?;

            // This is used to populate the database (and the InstanceInterfaceConfig
            // ip_addrs) with the host IP, meaning, if the instance-allocated prefix
            // is a /32 IpNetwork, it will be the IP. If it's a /30 (say, for FNN), it
            // will grab the 4th IP (the 2nd IP of the 2nd /31) to be handed back
            // as the visibly-assigned IP address for the instance.
            let host_ip = get_host_ip(&allocated_prefix)?;
            self.ip_addrs.insert(prefix_id, host_ip);
            self.interface_prefixes.insert(prefix_id, allocated_prefix);

            addresses.push(IpNetwork::new(host_ip, allocated_prefix.prefix())?);
        }

        Ok(addresses)
    }
}

pub async fn allocate_svi_ip(
    // Note: This is a PgTransaction, not a PgConnection, because we will be doing table locking,
    // which must happen in a transaction.
    txn: &mut PgTransaction<'_>,
    segment: &NetworkSegment,
) -> DatabaseResult<(NetworkPrefixId, IpAddr)> {
    let dhcp_handler: Box<dyn UsedIpResolver<PgConnection> + Send> =
        Box::new(UsedOverlayNetworkIpResolver {
            segment_id: segment.id,
            busy_ips: vec![],
        });

    // If either requested addresses are auto-generated, we lock the entire table.
    lock_table_for_allocation(txn.as_mut()).await?;

    let mut addresses_allocator = IpAllocator::new(
        txn.as_mut(),
        segment,
        dhcp_handler,
        AddressSelectionStrategy::NextAvailableIp,
    )
    .await?;
    match addresses_allocator.next() {
        Some((id, Ok(address))) => Ok((id, address.ip())),
        Some((_, Err(err))) => Err(err),
        _ => Err(DatabaseError::ResourceExhausted(format!(
            "Unable to allocate SVI IP for : No free IPs in segment {}.",
            segment.id
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use carbide_test_support::query_counter::count_queries;
    use carbide_uuid::vpc::VpcId;
    use chrono::Utc;
    use config_version::{ConfigVersion, Versioned};
    use model::instance::config::network::{InstanceInterfaceConfig, InterfaceFunctionId};
    use model::network_segment::{NetworkSegmentConfig, NetworkSegmentStatus, NetworkSegmentType};
    use uuid::Uuid;

    use super::*;

    fn create_valid_validation_data() -> Vec<NetworkSegment> {
        let vpc_id = VpcId::from_str("11609f10-c11d-1101-3261-6293ea0c0100").unwrap();
        let network_segments: Vec<NetworkSegment> = InterfaceFunctionId::iter_all()
            .enumerate()
            .map(|(idx, _function_id)| {
                let id: NetworkSegmentId =
                    Uuid::from_u128(BASE_SEGMENT_ID.as_u128() + idx as u128).into();
                let version = ConfigVersion::initial();
                NetworkSegment {
                    id,
                    version,
                    config: NetworkSegmentConfig {
                        name: id.to_string(),
                        subdomain_id: None,
                        vpc_id: Some(vpc_id),
                        mtu: 1500,
                        segment_type: NetworkSegmentType::Tenant,
                        allocation_strategy: Default::default(),
                        infer_slaac_eui64_addresses: false,
                    },
                    status: NetworkSegmentStatus {
                        controller_state: Versioned {
                            value: NetworkSegmentControllerState::Ready,
                            version,
                        },
                        controller_state_outcome: None,
                        history: Vec::new(),
                        vlan_id: None,
                        vni: None,
                        can_stretch: None,
                    },
                    created: Utc::now(),
                    updated: Utc::now(),
                    deleted: None,
                    prefixes: Vec::new(),
                }
            })
            .collect_vec();

        network_segments
    }

    const BASE_SEGMENT_ID: uuid::Uuid = uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c0000");
    fn create_valid_network_config() -> InstanceNetworkConfig {
        let interfaces: Vec<InstanceInterfaceConfig> = InterfaceFunctionId::iter_all()
            .enumerate()
            .map(|(idx, function_id)| {
                let network_segment_id: NetworkSegmentId =
                    Uuid::from_u128(BASE_SEGMENT_ID.as_u128() + idx as u128).into();
                InstanceInterfaceConfig {
                    function_id,
                    network_segment_id: Some(network_segment_id),
                    network_details: Some(
                        model::instance::config::network::NetworkDetails::NetworkSegment(
                            network_segment_id,
                        ),
                    ),
                    vpc_selection: None,
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                }
            })
            .collect();

        InstanceNetworkConfig {
            interfaces,
            auto_config: None,
        }
    }

    #[test]
    fn instance_address_segment_validation() {
        let data = create_valid_validation_data();
        let config = create_valid_network_config();
        let x = super::validate(&data, &config, &[], false);
        assert!(x.is_ok());
    }

    #[test]
    fn validate_missing_segment_in_db_fail() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data.swap_remove(10);
        assert!(super::validate(&data, &config, &[], false).is_err());
    }

    #[test]
    fn validate_multiple_vpc_must_fail() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data[0].config.vpc_id = Some(uuid::Uuid::new_v4().into());

        // Non-FNN and mixed VPCs still reject multi-VPC configs.
        assert!(super::validate(&data, &config, &[], false).is_err());
    }

    #[test]
    fn validate_multiple_fnn_vpc_must_pass() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data[0].config.vpc_id = Some(uuid::Uuid::new_v4().into());

        // FNN VPCs allow interfaces to span multiple VPCs.
        assert!(super::validate(&data, &config, &[], true).is_ok());
    }

    #[test]
    fn validate_missing_vpc_fail() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data[2].config.vpc_id = None;
        assert!(super::validate(&data, &config, &[], false).is_err());
    }

    #[test]
    fn validate_marked_deleted_segment_fail() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data[12].deleted = Some(Utc::now());
        assert!(super::validate(&data, &config, &[], false).is_err());
    }

    #[test]
    fn validate_not_ready_segment_fail() {
        let mut data = create_valid_validation_data();
        let config = create_valid_network_config();
        data[9].status.controller_state.value = NetworkSegmentControllerState::Provisioning;
        assert!(super::validate(&data, &config, &[], false).is_err());
    }

    // --- DB-backed batched-INSERT tests ---------------------------------
    //
    // These verify that `insert_instance_addresses` writes every accumulated
    // row with a SINGLE statement (the batching win), and that the persisted
    // rows match the input exactly (correctness). The insert helper is what
    // `allocate` funnels every interface/address row through, so measuring it
    // measures the lock-window reduction directly: one INSERT regardless of
    // how many addresses an instance's interfaces carry.

    /// Inserts one VPC and tenant network segment for an address fixture.
    async fn seed_vpc_and_segment(
        conn: &mut PgConnection,
        fixture_name: &str,
    ) -> (NetworkSegmentId, VpcId) {
        let vpc_id: VpcId =
            sqlx::query_scalar("INSERT INTO vpcs (name, version) VALUES ($1, $2) RETURNING id")
                .bind(format!("vpc-{fixture_name}"))
                .bind("1")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version, network_segment_type, vpc_id)
             VALUES ($1, $2, $3::network_segment_type_t, $4) RETURNING id",
        )
        .bind(format!("seg-{fixture_name}"))
        .bind("1")
        .bind("tenant")
        .bind(vpc_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        (segment_id, vpc_id)
    }

    /// Inserts the minimal FK ancestry `instance_addresses` requires: one VPC,
    /// one machine, one instance, and one tenant network segment.
    ///
    /// Mirrors the proven raw-INSERT fixture in `dns::resource_record`'s tests:
    /// only NOT-NULL columns without a default are supplied.
    async fn seed_fk_fixtures(
        conn: &mut PgConnection,
        fixture_name: &str,
    ) -> (InstanceId, NetworkSegmentId, VpcId) {
        let (segment_id, vpc_id) = seed_vpc_and_segment(conn, fixture_name).await;
        let machine_id = format!("test-machine-{fixture_name}");
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(&machine_id)
            .execute(&mut *conn)
            .await
            .unwrap();
        let instance_id: InstanceId =
            sqlx::query_scalar("INSERT INTO instances (machine_id) VALUES ($1) RETURNING id")
                .bind(&machine_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        (instance_id, segment_id, vpc_id)
    }

    /// Builds `k` distinct rows (sequential /32 host addresses) for one
    /// instance/segment/vpc, deriving the hostname exactly as `allocate` does.
    fn make_rows(
        instance_id: InstanceId,
        segment_id: NetworkSegmentId,
        vpc_id: VpcId,
        k: usize,
    ) -> Vec<InstanceAddress> {
        (0..k)
            .map(|i| {
                let ip = IpAddr::V4(std::net::Ipv4Addr::new(10, 3, 2, (i + 1) as u8));
                InstanceAddress {
                    instance_id,
                    address: ip,
                    segment_id,
                    prefix: IpNetwork::new(ip, 32).unwrap(),
                    vpc_id,
                    hostname: Some(crate::host_naming::address_to_hostname(&ip).unwrap()),
                }
            })
            .collect()
    }

    /// The unbatched baseline: one INSERT per row, exactly what `allocate`'s
    /// inner loop used to issue. Used only to establish the BEFORE count.
    async fn insert_one_at_a_time(
        conn: &mut PgConnection,
        rows: &[InstanceAddress],
    ) -> DatabaseResult<()> {
        let query = "INSERT INTO instance_addresses \
            (instance_id, address, segment_id, prefix, vpc_id, hostname) \
            VALUES ($1::uuid, $2, $3::uuid, $4::cidr, $5::uuid, $6)";
        for row in rows {
            sqlx::query(query)
                .bind(row.instance_id)
                .bind(row.address)
                .bind(row.segment_id)
                .bind(row.prefix)
                .bind(row.vpc_id)
                .bind(&row.hostname)
                .execute(&mut *conn)
                .await
                .map_err(|e| DatabaseError::query(query, e))?;
        }
        Ok(())
    }

    /// Large releases use one fixed-cost ordered lock query and one
    /// array-backed delete instead of expanding into one bind pair per address.
    #[crate::sqlx_test]
    async fn delete_addresses_for_instance_uses_one_array_delete(pool: sqlx::PgPool) {
        let addresses =
            vec![(NetworkSegmentId::new(), "10.88.0.20".parse().unwrap()); BIND_LIMIT / 2 + 1];

        let ((), query_count) = count_queries(async {
            let mut txn = pool.begin().await.unwrap();
            delete_addresses_for_instance(txn.as_mut(), InstanceId::new(), &addresses)
                .await
                .unwrap();
        })
        .await;

        assert_eq!(
            query_count, 2,
            "the release should use one lock query and one array delete"
        );
    }

    /// BEFORE/AFTER measurement of the INSERT statement count.
    ///
    /// K addresses go in two ways under a `sqlx::query`-event counter:
    ///   * BEFORE = one-INSERT-per-row loop  -> K statements (bite-check: > 1)
    ///   * AFTER  = `insert_instance_addresses` (batched) -> exactly 1 statement
    ///
    /// The `assert_eq!(after, 1)` is the regression guard: if the batched path
    /// ever regresses to per-row INSERTs, this test fails.
    #[crate::sqlx_test]
    async fn insert_instance_addresses_batches_to_one_statement(pool: sqlx::PgPool) {
        const K: usize = 5;

        // Fixtures are committed once and shared by both measured paths; each
        // path's addresses roll back with its own transaction, so BEFORE and
        // AFTER insert the same rows against the same clean slate. Each
        // counted future opens its transaction inside (BEGIN is queued
        // without an executed statement, adding nothing to the count) and
        // returns it, so the persisted-count check and the rollback happen
        // outside the counted region.
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) = seed_fk_fixtures(txn.as_mut(), "batch-count").await;
        txn.commit().await.unwrap();
        let rows = make_rows(instance_id, segment_id, vpc_id, K);

        // --- BEFORE: unbatched loop ---
        let (before_count, before_persisted) = {
            let (mut txn, before_count) = count_queries(async {
                let mut txn = pool.begin().await.unwrap();
                insert_one_at_a_time(txn.as_mut(), &rows).await.unwrap();
                txn
            })
            .await;

            let persisted: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM instance_addresses WHERE instance_id = $1",
            )
            .bind(instance_id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap();
            // Roll the addresses back so AFTER starts clean.
            txn.rollback().await.unwrap();
            (before_count, persisted)
        };

        // --- AFTER: batched helper ---
        let (after_count, after_persisted) = {
            let (mut txn, after_count) = count_queries(async {
                let mut txn = pool.begin().await.unwrap();
                insert_instance_addresses(txn.as_mut(), &rows)
                    .await
                    .unwrap();
                txn
            })
            .await;

            let persisted: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM instance_addresses WHERE instance_id = $1",
            )
            .bind(instance_id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap();
            txn.rollback().await.unwrap();
            (after_count, persisted)
        };

        println!(
            "instance_addresses INSERT statements for {K} addresses: BEFORE={before_count} \
             AFTER={after_count} (delta={})",
            before_count as i64 - after_count as i64,
        );

        // Bite-check: the unbatched path really did issue more than one INSERT.
        assert!(
            before_count > 1,
            "bite-check failed: unbatched insert issued {before_count} statements, expected > 1"
        );
        assert_eq!(
            before_count, K,
            "unbatched path should issue one INSERT per address"
        );
        // Regression guard: the batched path issues exactly one INSERT. The
        // helper only splits into multiple statements above BIND_LIMIT / 6
        // rows, far beyond any real allocation, so K = 5 is a single chunk.
        assert_eq!(
            after_count, 1,
            "batched insert_instance_addresses must issue exactly one statement"
        );

        // Both paths persist the same number of rows.
        assert_eq!(before_persisted, K as i64);
        assert_eq!(after_persisted, K as i64);
    }

    /// Correctness: `insert_instance_addresses` persists every row with the
    /// exact address / segment / vpc / hostname it was handed.
    #[crate::sqlx_test]
    async fn insert_instance_addresses_persists_all_rows(pool: sqlx::PgPool) {
        const K: usize = 4;
        let mut txn = pool.begin().await.unwrap();
        let (instance_id, segment_id, vpc_id) =
            seed_fk_fixtures(txn.as_mut(), "persisted-rows").await;
        let rows = make_rows(instance_id, segment_id, vpc_id, K);

        insert_instance_addresses(txn.as_mut(), &rows)
            .await
            .unwrap();

        // Read every persisted row back, keyed by address, and compare fields.
        let persisted: Vec<(IpNetwork, IpNetwork, NetworkSegmentId, VpcId, String)> =
            sqlx::query_as(
                "SELECT address, prefix, segment_id, vpc_id, hostname \
                 FROM instance_addresses WHERE instance_id = $1 ORDER BY address",
            )
            .bind(instance_id)
            .fetch_all(txn.as_mut())
            .await
            .unwrap();

        assert_eq!(persisted.len(), rows.len(), "row count mismatch");

        let by_ip: HashMap<IpAddr, &InstanceAddress> =
            rows.iter().map(|r| (r.address, r)).collect();
        for (address, prefix, seg, vpc, hostname) in &persisted {
            let expected = by_ip
                .get(&address.ip())
                .unwrap_or_else(|| panic!("unexpected address persisted: {}", address.ip()));
            assert_eq!(*seg, expected.segment_id, "segment_id mismatch");
            assert_eq!(*vpc, expected.vpc_id, "vpc_id mismatch");
            assert_eq!(
                Some(hostname.as_str()),
                expected.hostname.as_deref(),
                "hostname mismatch"
            );
            assert_eq!(prefix.ip(), expected.address, "prefix host mismatch");
        }

        txn.rollback().await.unwrap();
    }

    /// The empty-input fast path issues no statement at all. The transaction
    /// lives inside the counted future: neither its queued BEGIN nor its
    /// drop-rollback executes a statement, so the count stays at zero.
    #[crate::sqlx_test]
    async fn insert_instance_addresses_empty_is_noop(pool: sqlx::PgPool) {
        let ((), count) = count_queries(async {
            let mut txn = pool.begin().await.unwrap();
            insert_instance_addresses(txn.as_mut(), &[]).await.unwrap();
        })
        .await;
        assert_eq!(count, 0, "empty insert should issue no statements");
    }
}

#[cfg(test)]
mod segment_has_allocations_tests {
    use carbide_test_support::query_counter::count_queries;
    use model::network_prefix::NewNetworkPrefix;
    use model::network_segment::{
        AllocationStrategy, NetworkSegmentControllerState, NetworkSegmentType, NewNetworkSegment,
    };

    use super::*;

    /// Seeds a Ready segment plus one machine interface bound to it, so both the
    /// old count path and the new EXISTS path see an allocation.
    async fn seed_segment_with_interface(pool: &sqlx::PgPool) -> NetworkSegmentId {
        let mut txn = pool.begin().await.unwrap();
        let segment_id: NetworkSegmentId = uuid::Uuid::new_v4().into();
        network_segment::persist(
            NewNetworkSegment {
                id: segment_id,
                name: format!("seg-{segment_id}"),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![NewNetworkPrefix {
                    prefix: "10.9.0.0/24".parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 1,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Underlay,
                can_stretch: Some(false),
                allocation_strategy: AllocationStrategy::Reserved,
                infer_slaac_eui64_addresses: false,
            },
            txn.deref_mut(),
            NetworkSegmentControllerState::Ready,
        )
        .await
        .expect("seed segment");

        // A machine interface bound to the segment is one form of allocation.
        sqlx::query(
            "INSERT INTO machine_interfaces (segment_id, mac_address, primary_interface, hostname) \
             VALUES ($1, '02:00:00:00:00:01'::macaddr, true, 'drain-test-host')",
        )
        .bind(segment_id)
        .execute(txn.deref_mut())
        .await
        .expect("seed machine interface");

        txn.commit().await.unwrap();
        segment_id
    }

    /// Bite-check the round-trip win: the old drain check issued two `count(*)`
    /// queries (one per table) to compute a boolean, whereas
    /// `segment_has_allocations` answers it in a single query. Measures 2 vs 1.
    #[crate::sqlx_test]
    async fn segment_has_allocations_is_one_round_trip(pool: sqlx::PgPool) {
        let segment_id = seed_segment_with_interface(&pool).await;

        // Old path: machine_interface::count_by_segment_id + instance_address::count_by_segment_id.
        let seg = segment_id;
        let pool_ref = &pool;
        let ((), old_queries) = count_queries(async {
            let mut txn = pool_ref.begin().await.unwrap();
            let mi = crate::machine_interface::count_by_segment_id(&mut txn, &seg)
                .await
                .unwrap();
            let ia = count_by_segment_id(&mut txn, &seg).await.unwrap();
            // The allocation we seeded is visible to the old summed check.
            assert!(mi + ia > 0, "seeded interface should register");
        })
        .await;

        // New path: a single EXISTS-OR-EXISTS query.
        let (has, new_queries) = count_queries(async {
            let mut txn = pool_ref.begin().await.unwrap();
            segment_has_allocations(&mut txn, &seg).await.unwrap()
        })
        .await;

        assert!(has, "segment_has_allocations must see the seeded interface");
        assert_eq!(old_queries, 2, "old drain check issued two count queries");
        assert_eq!(new_queries, 1, "segment_has_allocations issues one query");
    }

    /// A segment with no normalized or configuration allocation reports false
    /// in the same one round trip as a populated segment.
    #[crate::sqlx_test]
    async fn segment_has_allocations_false_when_empty(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let segment_id: NetworkSegmentId = uuid::Uuid::new_v4().into();
        network_segment::persist(
            NewNetworkSegment {
                id: segment_id,
                name: format!("empty-{segment_id}"),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![NewNetworkPrefix {
                    prefix: "10.9.1.0/24".parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 1,
                }],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Underlay,
                can_stretch: Some(false),
                allocation_strategy: AllocationStrategy::Reserved,
                infer_slaac_eui64_addresses: false,
            },
            txn.deref_mut(),
            NetworkSegmentControllerState::Ready,
        )
        .await
        .expect("seed empty segment");

        let (has, queries) = count_queries(segment_has_allocations(&mut txn, &segment_id)).await;
        let has = has.unwrap();
        assert!(!has, "empty segment has no allocations");
        assert_eq!(queries, 1, "empty allocation check issues one query");
    }

    /// An allocation via `instance_addresses` alone -- no machine interface --
    /// also reports the segment as allocated, covering the predicate's second
    /// `EXISTS` arm. No `network_segments` row is needed for this normalized
    /// allocation case.
    #[crate::sqlx_test]
    async fn segment_has_allocations_sees_instance_addresses(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        let segment_id: NetworkSegmentId = uuid::Uuid::new_v4().into();

        let machine_id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn.deref_mut())
            .await
            .expect("seed machine");
        let instance_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO instances (id, machine_id) VALUES (gen_random_uuid(), $1) RETURNING id",
        )
        .bind(machine_id)
        .fetch_one(txn.deref_mut())
        .await
        .expect("seed instance");
        let vpc_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO vpcs (name, version) VALUES ('drain-test-vpc', 'v1') RETURNING id",
        )
        .fetch_one(txn.deref_mut())
        .await
        .expect("seed vpc");
        sqlx::query(
            "INSERT INTO instance_addresses (instance_id, address, prefix, segment_id, vpc_id) \
             VALUES ($1, '10.9.2.10'::inet, '10.9.2.0/24'::cidr, $2, $3)",
        )
        .bind(instance_id)
        .bind(segment_id)
        .bind(vpc_id)
        .execute(txn.deref_mut())
        .await
        .expect("seed instance address");

        let has = segment_has_allocations(&mut txn, &segment_id)
            .await
            .unwrap();
        assert!(has, "an instance address alone marks the segment allocated");
    }
}

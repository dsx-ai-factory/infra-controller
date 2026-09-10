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
use std::net::IpAddr;

use carbide_uuid::machine::HostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use futures::StreamExt;
use ipnetwork::IpNetwork;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::network_segment::{
    NetworkDefinition, NetworkSegment, NetworkSegmentControllerState, NetworkSegmentSearchConfig,
    NetworkSegmentType, NewNetworkSegment,
};
use sqlx::{PgConnection, PgTransaction};

use crate::db_read::DbReader;
use crate::instance_address::UsedOverlayNetworkIpResolver;
use crate::ip_allocator::{IpAllocator, UsedIpResolver};
use crate::machine_interface::UsedAdminNetworkIpResolver;
use crate::{
    ColumnInfo, DatabaseError, DatabaseResult, FilterableQueryBuilder, ObjectColumnFilter,
};

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = NetworkSegment;
    type ColumnType = NetworkSegmentId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Copy, Clone)]
pub struct VpcColumn;
impl ColumnInfo<'_> for VpcColumn {
    type TableType = NetworkSegment;
    type ColumnType = VpcId;

    fn column_name(&self) -> &'static str {
        "vpc_id"
    }
}

macro_rules! network_segment_snapshot_query {
    () => {
        r#"
     SELECT
        ns.*,
        COALESCE(prefixes_agg.json, '[]'::json) AS prefixes
     FROM network_segments ns
     LEFT JOIN LATERAL (
        SELECT np.segment_id,
            json_agg(np.*) AS json
        FROM network_prefixes np
        WHERE np.segment_id = ns.id
        GROUP BY np.segment_id
     ) AS prefixes_agg ON true
"#
    };
}

macro_rules! network_segment_snapshot_with_history_query {
    () => {
        r#"
     SELECT
        ns.*,
        COALESCE(prefixes_agg.json, '[]'::json) AS prefixes,
        COALESCE(history_agg.json, '[]'::json) AS history
     FROM network_segments ns
     LEFT JOIN LATERAL (
        SELECT np.segment_id,
            json_agg(np.*) AS json
        FROM network_prefixes np
        WHERE np.segment_id = ns.id
        GROUP BY np.segment_id
     ) AS prefixes_agg ON true
     LEFT JOIN LATERAL (
        SELECT h.object_id,
            json_agg(json_build_object('segment_id', h.object_id, 'state', h.state::text, 'state_version', h.state_version, 'timestamp', h."timestamp")) AS json
        FROM network_segment_state_history h
        WHERE h.object_id = ns.id::text
        GROUP BY h.object_id
     ) AS history_agg ON true
"#
    };
}

pub async fn persist(
    value: NewNetworkSegment,
    txn: &mut PgConnection,
    initial_state: NetworkSegmentControllerState,
) -> Result<NetworkSegment, DatabaseError> {
    let version = ConfigVersion::initial();

    let query = "INSERT INTO network_segments (
                id,
                name,
                subdomain_id,
                vpc_id,
                mtu,
                version,
                controller_state_version,
                controller_state,
                vlan_id,
                vni_id,
                network_segment_type,
                can_stretch,
                allocation_strategy,
                infer_slaac_eui64_addresses)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            RETURNING id";
    let segment_id: NetworkSegmentId = sqlx::query_as(query)
        .bind(value.id)
        .bind(&value.name)
        .bind(value.subdomain_id)
        .bind(value.vpc_id)
        .bind(value.mtu)
        .bind(version)
        .bind(version)
        .bind(sqlx::types::Json(&initial_state))
        .bind(value.vlan_id)
        .bind(value.vni)
        .bind(value.segment_type)
        .bind(value.can_stretch)
        .bind(value.allocation_strategy)
        .bind(value.infer_slaac_eui64_addresses)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    crate::network_prefix::create_for(txn, &segment_id, &value.prefixes).await?;
    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::NetworkSegment,
        &segment_id,
        &initial_state,
        version,
    )
    .await?;

    find_by(
        txn,
        ObjectColumnFilter::One(IdColumn, &segment_id),
        Default::default(),
    )
    .await?
    .pop()
    .ok_or_else(|| {
        DatabaseError::new(
            "finding just-created network segment",
            sqlx::Error::RowNotFound,
        )
    })
}

pub async fn for_vpc(
    txn: impl DbReader<'_>,
    vpc_id: VpcId,
) -> Result<Vec<NetworkSegment>, DatabaseError> {
    static QUERY: &str = concat!(
        network_segment_snapshot_query!(),
        " WHERE ns.vpc_id=$1::uuid"
    );
    let results: Vec<NetworkSegment> = {
        sqlx::query_as(QUERY)
            .bind(vpc_id)
            .fetch_all(txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e))?
    };

    Ok(results)
}

/// Returns the segment matched by a DHCP relay address.
///
/// Exact DHCPv6 link-address matches win over prefix containment, matching the
/// candidate ordering used by `for_relay_all`.
pub async fn for_relay(
    txn: &mut PgConnection,
    relay: IpAddr,
) -> DatabaseResult<Option<NetworkSegment>> {
    let mut results = for_relay_all(txn, std::slice::from_ref(&relay)).await?;

    match results.len() {
        0 | 1 => Ok(results.pop()),
        _ => {
            // DHCPv6 link-address equality is unique and more specific than
            // prefix containment, so it resolves the otherwise ambiguous match.
            results
                .into_iter()
                .find(|segment| {
                    segment
                        .prefixes
                        .iter()
                        .any(|prefix| prefix.dhcpv6_link_address == Some(relay))
                })
                .map(Some)
                .ok_or_else(|| {
                    DatabaseError::internal(format!(
                        "Multiple network segments defined for relay address {relay}"
                    ))
                })
        }
    }
}

/// Returns the segment whose managed prefix contains `address`.
///
/// This intentionally ignores `dhcpv6_link_address`: that field is DHCP relay
/// routing context and may be outside the segment prefix.
pub async fn for_prefix_containing_address(
    txn: &mut PgConnection,
    address: IpAddr,
) -> DatabaseResult<Option<NetworkSegment>> {
    static QUERY: &str = concat!(
        network_segment_snapshot_query!(),
        r#"
                WHERE EXISTS (
                    SELECT 1
                    FROM network_prefixes
                    WHERE network_prefixes.segment_id = ns.id
                    -- Static address ownership uses managed prefix containment only.
                    AND $1::inet <<= network_prefixes.prefix
                )
                ORDER BY ns.id"#,
    );
    let mut results: Vec<NetworkSegment> = sqlx::query_as(QUERY)
        .bind(IpNetwork::from(address))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;

    match results.len() {
        0 | 1 => Ok(results.pop()),
        _ => Err(DatabaseError::internal(format!(
            "Multiple network segments contain address {address}"
        ))),
    }
}

/// Resolve the managed segment whose configured prefix contains a static
/// address.
///
/// Unlike [`for_static_address`], this never falls back to
/// `static-assignments`. ExpectedInterface declarations using an explicit
/// allocation policy or a DPU role use this stricter lookup.
pub async fn for_managed_static_address(
    txn: &mut PgConnection,
    address: IpAddr,
    expected_segment_type: Option<NetworkSegmentType>,
) -> DatabaseResult<NetworkSegment> {
    let segment = for_prefix_containing_address(&mut *txn, address)
        .await?
        .ok_or_else(|| {
            DatabaseError::InvalidArgument(match expected_segment_type {
                Some(expected_segment_type) => format!(
                    "fixed IP {address} is not within a configured {expected_segment_type} network segment",
                ),
                None => {
                    format!("fixed IP {address} is not within a configured network segment")
                }
            })
        })?;

    if let Some(expected_segment_type) = expected_segment_type
        && segment.config.segment_type != expected_segment_type
    {
        return Err(DatabaseError::InvalidArgument(format!(
            "fixed IP {address} belongs to {} network segment {}, not the expected {expected_segment_type} segment type",
            segment.config.segment_type, segment.config.name,
        )));
    }

    Ok(segment)
}

/// Resolve the segment that owns a configured static address.
///
/// Addresses outside managed prefixes use `static-assignments`.
pub async fn for_static_address(
    txn: &mut PgConnection,
    address: IpAddr,
) -> DatabaseResult<NetworkSegment> {
    match for_prefix_containing_address(&mut *txn, address).await? {
        Some(segment) => Ok(segment),
        None => static_assignments(&mut *txn).await,
    }
}

/// Returns all network segments that contain at least one relay/gateway IP.
pub async fn for_relay_all(
    txn: &mut PgConnection,
    relays: &[IpAddr],
) -> DatabaseResult<Vec<NetworkSegment>> {
    static QUERY: &str = concat!(
        network_segment_snapshot_query!(),
        r#"
                WHERE EXISTS (
                    SELECT 1
                    FROM network_prefixes
                    WHERE network_prefixes.segment_id = ns.id
                    AND (
                        -- Relay candidates match either normal prefix containment...
                        EXISTS (
                            SELECT 1 FROM unnest($1::inet[]) AS ip
                            WHERE ip <<= network_prefixes.prefix
                        )
                        -- ...or exact DHCPv6 link-address metadata.
                        OR network_prefixes.dhcpv6_link_address = ANY($1::inet[])
                    )
                )
                -- Exact DHCPv6 link-address matches sort first so callers see
                -- the authoritative segment before prefix fallback candidates.
                ORDER BY EXISTS (
                    SELECT 1
                    FROM network_prefixes
                    WHERE network_prefixes.segment_id = ns.id
                    AND network_prefixes.dhcpv6_link_address = ANY($1::inet[])
                ) DESC,
                ns.id"#,
    );
    let results = sqlx::query_as(QUERY)
        .bind(
            relays
                .iter()
                .map(|v| IpNetwork::from(v.to_owned()))
                .collect::<Vec<IpNetwork>>(),
        )
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;

    Ok(results)
}

/// Accepts a set of relay/gateway IPs and a segment type and returns
/// all network segments that match.
pub async fn for_segment_type_all(
    txn: &mut PgConnection,
    relays: &[IpAddr],
    segment_type: NetworkSegmentType,
) -> DatabaseResult<Vec<NetworkSegment>> {
    static QUERY: &str = concat!(
        network_segment_snapshot_query!(),
        r#"
                WHERE EXISTS (
                    SELECT 1
                    FROM network_prefixes
                    WHERE network_prefixes.segment_id = ns.id
                    AND (
                        -- Relay candidates match either normal prefix containment...
                        EXISTS (
                            SELECT 1 FROM unnest($1::inet[]) AS ip
                            WHERE ip <<= network_prefixes.prefix
                        )
                        -- ...or exact DHCPv6 link-address metadata.
                        OR network_prefixes.dhcpv6_link_address = ANY($1::inet[])
                    )
                )
                -- Apply requested segment-type narrowing after relay ownership matching.
                AND $2 = ns.network_segment_type
                -- Exact DHCPv6 link-address matches sort first so callers see
                -- the authoritative segment before prefix fallback candidates.
                ORDER BY EXISTS (
                    SELECT 1
                    FROM network_prefixes
                    WHERE network_prefixes.segment_id = ns.id
                    AND network_prefixes.dhcpv6_link_address = ANY($1::inet[])
                ) DESC,
                ns.id"#,
    );

    let results = sqlx::query_as(QUERY)
        .bind(
            relays
                .iter()
                .map(|v| IpNetwork::from(v.to_owned()))
                .collect::<Vec<IpNetwork>>(),
        )
        .bind(segment_type)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new(QUERY, e))?;

    Ok(results)
}

/// Retrieves the IDs of all network segments.
/// If `segment_type` is specified, only IDs of segments that match the specific type are returned.
pub async fn list_segment_ids(
    txn: &mut PgConnection,
    segment_type: Option<NetworkSegmentType>,
) -> Result<Vec<NetworkSegmentId>, DatabaseError> {
    let (query, mut segment_id_stream) = if let Some(segment_type) = segment_type {
        let query = "SELECT id FROM network_segments where network_segment_type=$1";
        let stream = sqlx::query_as(query).bind(segment_type).fetch(txn);
        (query, stream)
    } else {
        let query = "SELECT id FROM network_segments";
        let stream = sqlx::query_as(query).fetch(txn);
        (query, stream)
    };

    let mut results = Vec::new();
    while let Some(maybe_id) = segment_id_stream.next().await {
        let id = maybe_id.map_err(|e| DatabaseError::query(query, e))?;
        results.push(id);
    }

    Ok(results)
}
/// Fetch the stored definition for a single network, or `None` if never seeded.
pub async fn stored_def(
    txn: impl DbReader<'_>,
    name: &str,
) -> Result<Option<NetworkDefinition>, DatabaseError> {
    let query = "SELECT definition FROM network_def WHERE name = $1";
    let row: Option<(sqlx::types::Json<NetworkDefinition>,)> = sqlx::query_as(query)
        .bind(name)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(row.map(|(json,)| json.0))
}

/// Fetch every stored network definition as a `HashMap<name, def>`.
pub async fn all_stored_defs(
    txn: impl DbReader<'_>,
) -> Result<HashMap<String, NetworkDefinition>, DatabaseError> {
    let query = "SELECT name, definition FROM network_def";
    let rows: Vec<(String, sqlx::types::Json<NetworkDefinition>)> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(rows
        .into_iter()
        .map(|(name, json)| (name, json.0))
        .collect())
}

/// Insert the `NetworkDefinition` snapshot for a network that has never
/// been seeded. Callers must check with `stored_def` / `all_stored_defs`
/// before calling this, and skip the insert when a snapshot is present.
///
/// `segment_id` must reference an existing row in `network_segments`.
pub async fn insert_network_def(
    txn: &mut PgConnection,
    name: &str,
    segment_id: NetworkSegmentId,
    def: &NetworkDefinition,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO network_def (name, segment_id, definition, seeded_at) \
                 VALUES ($1, $2, $3, NOW())";
    let definition = serde_json::to_value(def).map_err(|e| {
        DatabaseError::InvalidArgument(format!(
            "NetworkDefinition: {def:?} could not be serialized to JSON: {e}"
        ))
    })?;

    sqlx::query(query)
        .bind(name)
        .bind(segment_id)
        .bind(definition)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn segment_exists(txn: &mut PgConnection, name: &str) -> Result<bool, DatabaseError> {
    let query = "SELECT EXISTS(SELECT 1 FROM network_segments WHERE name = $1)";
    sqlx::query_scalar(query)
        .bind(name)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Reconcile declared network definitions against what was previously seeded.
///
///   1. **New** (no snapshot, no segment): returned to the caller for creation.
///      The caller is responsible for creating the segment and writing the
///      snapshot in the same transaction.
///   2. **Backfill** (no snapshot, segment present): record the snapshot,
///      linking it to the existing segment's id.
///   3. **In sync** (snapshot matches declaration): no-op.
///   4. **Drift** (snapshot differs from declaration, segment present):
///      warn and leave both in place. Operator must reconcile by hand.
///
/// Networks that appear in the snapshot table but are no longer declared
/// ("dropped" from `InitialObjectsConfig.networks`) are warned about, but
/// not removed.
///
/// # Returns
///
/// Config-declared networks that have neither a segment row nor a snapshot,
/// in unspecified order. The caller must create the segment and write the
/// snapshot in the same transaction.
pub async fn reconcile_network_defs(
    txn: &mut PgConnection,
    declared: &HashMap<String, NetworkDefinition>,
) -> Result<Vec<(String, NetworkDefinition)>, DatabaseError> {
    let stored = all_stored_defs(&mut *txn).await?;
    let mut to_create: Vec<(String, NetworkDefinition)> = Vec::new();

    for (name, def) in declared {
        let exists = segment_exists(&mut *txn, name).await?;
        match (stored.get(name), exists) {
            // Already seeded with the current declaration — nothing to do.
            (Some(stored_def), true) if stored_def == def => {}
            // Declaration has drifted since seed; warn and leave both in place.
            (Some(stored_def), true) => {
                tracing::warn!(
                    network_name = name,
                    stored = ?stored_def,
                    declared = ?def,
                    "NetworkDefinition has changed since it was seeded; not re-applying"
                );
            }
            // Network segment exists, but has no snapshot yet.
            // Pre-migration deployment or a network was re-added after a
            // snapshot was manually deleted.
            //
            // TOML-declared names are unique by nature, but
            // `network_segments.name` has no UNIQUE constraint at the DB
            // layer — multiple segments can share a name. If we
            // see more than one match, we can't tell which one the
            // operator originally seeded, so we skip the backfill and
            // log; the operator can resolve by inserting `network_def`
            // by hand.
            (None, true) => {
                let query = "SELECT id FROM network_segments WHERE name = $1 LIMIT 2";
                let candidates: Vec<NetworkSegmentId> = sqlx::query_scalar(query)
                    .bind(name)
                    .fetch_all(&mut *txn)
                    .await
                    .map_err(|e| DatabaseError::query(query, e))?;
                if let [segment_id] = candidates.as_slice() {
                    insert_network_def(txn, name, *segment_id, def).await?;
                    tracing::info!(
                        network_name = name,
                        "Backfilled NetworkDefinition snapshot for pre-existing network segment"
                    );
                } else {
                    tracing::warn!(
                        network_name = name,
                        matching_network_segment_count = candidates.len(),
                        "Backfill skipped: multiple network_segments share this name; \
                         operator must reconcile by hand",
                    );
                }
            }
            // No segment and no snapshot: return to the caller for creation.
            (None, false) => {
                to_create.push((name.clone(), def.clone()));
            }
            (Some(_), false) => {
                unreachable!("network_def.segment_id is FK; snapshot cannot outlive its segment")
            }
        }
    }

    for name in stored.keys() {
        if !declared.contains_key(name) {
            tracing::warn!(
                network_name = name,
                "Network segment exists in database but is no longer declared in any config file"
            );
        }
    }

    Ok(to_create)
}
pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::network_segment::NetworkSegmentSearchFilter,
) -> Result<Vec<NetworkSegmentId>, DatabaseError> {
    // build query
    let mut builder = sqlx::QueryBuilder::new("SELECT s.id FROM network_segments AS s");
    let mut has_filter = false;
    if let Some(tenant_org_id) = &filter.tenant_org_id {
        builder.push(" JOIN vpcs AS v ON s.vpc_id = v.id WHERE v.organization_id = ");
        builder.push_bind(tenant_org_id);
        has_filter = true;
    }
    if let Some(name) = &filter.name {
        if has_filter {
            builder.push(" AND s.name = ");
        } else {
            builder.push(" WHERE s.name = ");
        }
        builder.push_bind(name);
    }

    let query = builder.build_query_as();
    let ids: Vec<NetworkSegmentId> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("network_segment::find_ids", e))?;

    Ok(ids)
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = NetworkSegment>, DB>(
    conn: &mut DB,
    filter: ObjectColumnFilter<'a, C>,
    search_config: NetworkSegmentSearchConfig,
) -> Result<Vec<NetworkSegment>, DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let mut query = FilterableQueryBuilder::new(if search_config.include_history {
        network_segment_snapshot_with_history_query!()
    } else {
        network_segment_snapshot_query!()
    })
    .filter(&filter);

    let mut all_records = query
        .build_query_as()
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    if search_config.include_num_free_ips {
        update_num_free_ips_into_prefix_list(conn, &mut all_records).await?;
    }
    Ok(all_records)
}

/// Find network segments attached to a machine through machine_interfaces, optionally of a certain type
pub async fn find_ids_by_machine_id(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    network_segment_type: Option<NetworkSegmentType>,
) -> Result<Vec<NetworkSegmentId>, DatabaseError> {
    let result = batch_find_ids_by_machine_ids(txn, &[*machine_id], network_segment_type).await?;

    Ok(result.get(machine_id).cloned().unwrap_or_default())
}

/// Batch find network segments attached to multiple machines through machine_interfaces.
/// Returns a HashMap mapping each machine ID to its list of segment IDs.
pub async fn batch_find_ids_by_machine_ids(
    txn: &mut PgConnection,
    machine_ids: &[HostMachineId],
    network_segment_type: Option<NetworkSegmentType>,
) -> Result<HashMap<HostMachineId, Vec<NetworkSegmentId>>, DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut query = sqlx::QueryBuilder::new(
        r#"SELECT mi.machine_id, ns.id FROM machines m
                LEFT JOIN machine_interfaces mi ON (mi.machine_id = m.id)
                INNER JOIN network_segments ns ON (ns.id = mi.segment_id)
                WHERE mi.machine_id = ANY("#,
    );

    query.push_bind(
        machine_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>(),
    );
    query.push(")");
    query.push(" AND mi.interface_type != 'Bmc'");

    if let Some(network_segment_type) = network_segment_type {
        query
            .push(" AND ns.network_segment_type = ")
            .push_bind(network_segment_type);
    }

    let rows: Vec<(String, NetworkSegmentId)> = query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))?;

    let mut result: HashMap<HostMachineId, Vec<NetworkSegmentId>> = HashMap::new();
    for (machine_id_str, segment_id) in rows {
        if let Ok(machine_id) = machine_id_str.parse::<HostMachineId>() {
            result.entry(machine_id).or_default().push(segment_id);
        }
    }

    Ok(result)
}

async fn update_num_free_ips_into_prefix_list<DB>(
    conn: &mut DB,
    all_records: &mut [NetworkSegment],
) -> Result<(), DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    for record in all_records.iter_mut().filter(|s| !s.prefixes.is_empty()) {
        let mut busy_ips = vec![];
        for prefix in &record.prefixes {
            if let Some(svi_ip) = prefix.svi_ip {
                busy_ips.push(svi_ip);
            }
        }
        let dhcp_handler: Box<dyn UsedIpResolver<DB> + Send> =
            if record.config.segment_type.is_tenant() {
                // Note on UsedOverlayNetworkIpResolver:
                // In this case, the IpAllocator isn't being used to iterate to get
                // the next available prefix_length allocation -- it's actually just
                // being used to get the number of free IPs left in a given tenant
                // network segment, so just hard-code a /32 prefix_length. NOW.. on
                // one hand, you could say the prefix_length doesn't matter here,
                // because this is really just here to get the number of free IPs left
                // in a network segment. BUT, on the other hand, do we care about the
                // number of free IPs left, or the number of free instance allocations
                // left? For example, if we're allocating /30's, we might be more
                // interested in knowing we can allocate 4 more machines (and not 16
                // more IPs).
                Box::new(UsedOverlayNetworkIpResolver {
                    segment_id: record.id,
                    busy_ips,
                })
            } else {
                // Note on UsedAdminNetworkIpResolver:
                // In this case, the IpAllocator isn't being used to iterate to get
                // the next available prefix_length allocation -- it's actually just
                // being used to get the number of free IPs left in a given admin
                // network segment, so just hard-code a /32 prefix_length. Unlike the
                // tenant segments, the admin segments are always (at least for the
                // foreseeable future) just going to allocate a /32 for the machine
                // interface.
                Box::new(UsedAdminNetworkIpResolver {
                    segment_id: record.id,
                    busy_ips,
                })
            };

        let allocator = IpAllocator::new(
            &mut *conn,
            record,
            dhcp_handler,
            AddressSelectionStrategy::NextAvailableIp,
        )
        .await
        .map_err(|e| {
            DatabaseError::new(
                "IpAllocator.new error",
                sqlx::Error::Io(std::io::Error::other(e.to_string())),
            )
        })?;

        for prefix in &mut record.prefixes {
            prefix.num_free_ips = Some(allocator.num_free(prefix.id).map_err(|e| {
                DatabaseError::new(
                    "IpAllocator.num_free error",
                    sqlx::Error::Io(std::io::Error::other(e.to_string())),
                )
            })?);
        }
    }

    Ok(())
}

/// Updates the network segment state that is owned by the state controller
/// under the premise that the current controller state version didn't change.
///
/// Returns `true` if the state could be updated, and `false` if the object
/// either doesn't exist anymore or is at a different version.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    segment_id: NetworkSegmentId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &NetworkSegmentControllerState,
) -> Result<bool, DatabaseError> {
    let query = "UPDATE network_segments SET controller_state_version=$1, controller_state=$2::json where id=$3::uuid AND controller_state_version=$4 returning id";
    let result = sqlx::query_as::<_, NetworkSegmentId>(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(segment_id)
        .bind(expected_version)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result.is_some())
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    segment_id: NetworkSegmentId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query = "UPDATE network_segments SET controller_state_outcome=$1::json WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(segment_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn set_vpc_id_and_can_stretch(
    value: &NetworkSegment,
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE network_segments SET vpc_id=$1, can_stretch=true WHERE id=$2";
    sqlx::query(query)
        .bind(vpc_id)
        .bind(value.id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn attach_to_vpc(
    value: &NetworkSegment,
    txn: &mut PgConnection,
    vpc_id: VpcId,
) -> Result<NetworkSegment, DatabaseError> {
    let next_version = value.version.increment();
    let query = "UPDATE network_segments
            SET vpc_id=$1, version=$2, updated=NOW()
            WHERE id=$3 AND version=$4 AND deleted IS NULL
            RETURNING id";
    let updated_id: NetworkSegmentId = sqlx::query_as(query)
        .bind(vpc_id)
        .bind(next_version)
        .bind(value.id)
        .bind(value.version)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => DatabaseError::ConcurrentModificationError(
                "network_segment",
                value.version.to_string(),
            ),
            e => DatabaseError::query(query, e),
        })?;

    find_by(
        txn,
        ObjectColumnFilter::One(IdColumn, &updated_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .pop()
    .ok_or_else(|| {
        DatabaseError::new(
            "finding just-attached network segment",
            sqlx::Error::RowNotFound,
        )
    })
}

pub async fn mark_as_deleted(
    value: &NetworkSegment,
    txn: &mut PgConnection,
) -> DatabaseResult<NetworkSegmentId> {
    // This check is not strictly necessary here, since the segment state machine
    // will also wait until all allocated addresses have been freed before actually
    // deleting the segment. However it gives the user some early feedback for
    // the commmon case, which allows them to free resources
    let num_machine_interfaces =
        crate::machine_interface::count_by_segment_id(txn, &value.id).await?;
    if num_machine_interfaces > 0 {
        return DatabaseResult::Err(DatabaseError::NetworkSegmentDelete(
            "Network Segment can't be deleted with associated MachineInterface".to_string(),
        ));
    }
    let num_instance_addresses =
        crate::instance_address::count_by_segment_id(txn, &value.id).await?;
    if num_instance_addresses > 0 {
        return DatabaseResult::Err(DatabaseError::NetworkSegmentDelete(
            "Network Segment can't be deleted while addresses on the segment are allocated to instances".to_string(),
        ));
    }
    let num_referencing_instances =
        crate::instance::count_network_segment_references(txn, &value.id).await?;
    if num_referencing_instances > 0 {
        return DatabaseResult::Err(DatabaseError::NetworkSegmentDelete(format!(
            "network segment can't be deleted while referenced by network configurations; referencing instance count: {num_referencing_instances}"
        )));
    }

    let query = "UPDATE network_segments SET updated=NOW(), deleted=NOW() WHERE id=$1 RETURNING id";
    let id = sqlx::query_as(query)
        .bind(value.id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(id)
}

/// Physically deletes a segment after rechecking that no allocations reference it.
///
/// The caller's transaction retains the recheck's table lock through physical
/// deletion, serializing with the allocator's exclusive lock on
/// `instance_addresses`.
pub async fn final_delete(
    segment_id: NetworkSegmentId,
    txn: &mut PgTransaction<'_>,
) -> Result<NetworkSegmentId, DatabaseError> {
    // Recheck inside the delete transaction. The table lock serializes this
    // read against the allocator's exclusive lock on `instance_addresses`,
    // closing the gap between the controller's drain decision and physical
    // deletion.
    if crate::instance_address::segment_has_allocations(txn.as_mut(), &segment_id).await? {
        return Err(DatabaseError::NetworkSegmentDelete(
            "network segment can't be deleted while allocations still reference it".to_string(),
        ));
    }

    crate::network_prefix::delete_for_segment(segment_id, txn.as_mut()).await?;

    let query = "DELETE FROM network_segments WHERE id=$1::uuid RETURNING id";
    let segment: NetworkSegmentId = sqlx::query_as(query)
        .bind(segment_id)
        .fetch_one(txn.as_mut())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(segment)
}

pub async fn find_by_name(
    txn: &mut PgConnection,
    name: &str,
) -> Result<NetworkSegment, DatabaseError> {
    static QUERY: &str = concat!(network_segment_snapshot_query!(), " WHERE name = $1");
    sqlx::query_as(QUERY)
        .bind(name)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

/// Well-known name for the static assignments "anchor segment",
/// making it extra-obvious that it's a special one.
pub const STATIC_ASSIGNMENTS_SEGMENT_NAME: &str = "static-assignments";

/// Returns the static-assignments anchor segment, used for external
/// static IP assignments that don't fall within any managed network prefix.
pub async fn static_assignments(txn: &mut PgConnection) -> Result<NetworkSegment, DatabaseError> {
    find_by_name(txn, STATIC_ASSIGNMENTS_SEGMENT_NAME).await
}

/// Returns all admin network segments.
pub async fn admin(txn: &mut PgConnection) -> Result<Vec<NetworkSegment>, DatabaseError> {
    static QUERY: &str = concat!(
        network_segment_snapshot_query!(),
        " WHERE network_segment_type = 'admin' ORDER BY ns.id",
    );
    let segments: Vec<NetworkSegment> = sqlx::query_as(QUERY)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;

    if segments.is_empty() {
        return Err(DatabaseError::query(QUERY, sqlx::Error::RowNotFound));
    }

    Ok(segments)
}

/// Are queried segment in ready state?
/// Returns true if all segments are in Ready state, else false
pub async fn are_network_segments_ready<DB>(
    conn: &mut DB,
    segment_ids: &[NetworkSegmentId],
) -> Result<bool, DatabaseError>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let segments = find_by(
        conn,
        ObjectColumnFilter::List(IdColumn, segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;

    Ok(!segments
        .iter()
        .any(|x| x.status.controller_state.value != NetworkSegmentControllerState::Ready))
}

/// This function is different from `mark_as_deleted` as no validation is checked here and it
/// takes a list of ids to reduce db handling time.
/// Instance is already deleted immediately before this.
pub async fn mark_as_deleted_no_validation(
    txn: &mut PgConnection,
    network_segment_ids: &[NetworkSegmentId],
) -> DatabaseResult<NetworkSegmentId> {
    let query =
        "UPDATE network_segments SET updated=NOW(), deleted=NOW() WHERE id=ANY($1) RETURNING id";
    let id = sqlx::query_as(query)
        .bind(network_segment_ids)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(id)
}

/// SVI IP is needed for Network Segments attached to FNN VPCs.
/// Usually third IP of a prefix is used as SVI IP. In case, first 3 IPs are not reserved,
/// carbide will pick any available free IP and store it in DB for further use.
/// Allocates SVI IPs for all prefixes in a segment that don't already have one.
/// For dual-stack segments, this allocates one SVI IP per prefix (v4 and v6).
///
/// For each prefix, the SVI IP is the 3rd address in the prefix (e.g. 10.0.1.2
/// in 10.0.1.0/24, or 2001:db8::2 in 2001:db8::/112). This address is shared
/// across all DPUs via VRR.
pub async fn allocate_svi_ip(
    value: &NetworkSegment,
    txn: &mut PgTransaction<'_>,
) -> Result<IpAddr, DatabaseError> {
    let mut first_svi_ip = None;

    // A tenant prefix with fewer than three reserved addresses falls back to
    // the instance address allocator. Take its table lock before touching any
    // prefix row so this path uses the same lock order as segment deletion,
    // including dual-stack segments whose prefixes reserve different numbers
    // of addresses.
    if value.config.segment_type.is_tenant()
        && value
            .prefixes
            .iter()
            .any(|prefix| prefix.svi_ip.is_none() && prefix.num_reserved < 3)
    {
        crate::instance_address::lock_table_for_allocation(txn.as_mut()).await?;
    }

    for prefix in &value.prefixes {
        if prefix.svi_ip.is_some() {
            if first_svi_ip.is_none() {
                first_svi_ip = prefix.svi_ip;
            }
            continue;
        }

        // For prefixes with num_reserved >= 3, the 3rd IP is guaranteed reserved
        // and safe to use as the SVI IP. For smaller prefixes, the 3rd IP may
        // already be allocated to an instance, so we fall back to the IP allocator
        // to find the next free address in this specific prefix.
        let svi_ip = if prefix.num_reserved >= 3 {
            prefix.prefix.iter().nth(2).ok_or_else(|| {
                DatabaseError::internal(format!("Prefix {} does not have 3 valid IPs.", prefix.id))
            })?
        } else {
            // Build a single-prefix segment view so the allocator only looks
            // at this prefix (avoids the cross-prefix bug for dual-stack).
            let single_prefix_segment = NetworkSegment {
                prefixes: vec![prefix.clone()],
                ..value.clone()
            };
            let (_, svi_ip) = if !value.config.segment_type.is_tenant() {
                crate::machine_interface::allocate_svi_ip(txn, &single_prefix_segment).await?
            } else {
                crate::instance_address::allocate_svi_ip(txn, &single_prefix_segment).await?
            };
            svi_ip
        };

        crate::network_prefix::set_svi_ip(txn, prefix.id, &svi_ip).await?;

        if first_svi_ip.is_none() {
            first_svi_ip = Some(svi_ip);
        }
    }

    first_svi_ip.ok_or_else(|| DatabaseError::NotFoundError {
        kind: "prefix",
        id: value.id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use model::network_prefix::NewNetworkPrefix;
    use model::network_segment::NetworkDefinitionSegmentType;

    use super::*;

    // Insert just enough into `network_segments` to make
    // `segment_exists(name)` return true;
    async fn minimum_segment_data(
        pool: &sqlx::PgPool,
        name: &str,
    ) -> Result<NetworkSegmentId, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
    }

    /// Persists one test segment with a single prefix row.
    async fn persist_test_segment(
        pool: &sqlx::PgPool,
        name: &str,
        prefix: &str,
        gateway: Option<&str>,
        dhcpv6_link_address: Option<&str>,
    ) -> Result<NetworkSegmentId, Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let segment = NewNetworkSegment {
            id: uuid::Uuid::new_v4().into(),
            name: name.to_string(),
            subdomain_id: None,
            vpc_id: None,
            mtu: 1500,
            prefixes: vec![NewNetworkPrefix {
                prefix: prefix.parse()?,
                gateway: gateway.map(str::parse).transpose()?,
                dhcpv6_link_address: dhcpv6_link_address.map(str::parse).transpose()?,
                num_reserved: 1,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Admin,
            can_stretch: None,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
        };
        let segment_id = segment.id;

        persist(segment, &mut txn, NetworkSegmentControllerState::Ready).await?;
        txn.commit().await?;
        Ok(segment_id)
    }

    #[crate::sqlx_test]
    async fn free_ip_counts_are_populated_for_every_dual_stack_prefix(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let segment = persist(
            NewNetworkSegment {
                id: uuid::Uuid::new_v4().into(),
                name: "dual-stack-free-counts".to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                // Configure IPv6 first so the fixture does not imply an IPv4-first
                // contract. The assertions below find each family explicitly because
                // `json_agg` does not guarantee row order.
                prefixes: vec![
                    NewNetworkPrefix {
                        prefix: "2001:db8::/64".parse()?,
                        gateway: None,
                        dhcpv6_link_address: None,
                        num_reserved: 0,
                    },
                    NewNetworkPrefix {
                        prefix: "192.0.2.0/30".parse()?,
                        gateway: None,
                        dhcpv6_link_address: None,
                        num_reserved: 0,
                    },
                ],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Admin,
                can_stretch: None,
                allocation_strategy: Default::default(),
                infer_slaac_eui64_addresses: false,
            },
            &mut txn,
            NetworkSegmentControllerState::Ready,
        )
        .await?;

        let found_without_counts = find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &segment.id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?;
        assert_eq!(found_without_counts.len(), 1);
        assert!(
            found_without_counts[0]
                .prefixes
                .iter()
                .all(|prefix| prefix.num_free_ips.is_none())
        );

        let mut found = find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &segment.id),
            NetworkSegmentSearchConfig {
                include_num_free_ips: true,
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(found.len(), 1);
        let found = found.pop().expect("persisted segment should be returned");
        assert_eq!(found.id, segment.id);

        let ipv4 = found
            .prefixes
            .iter()
            .find(|prefix| prefix.prefix.is_ipv4())
            .expect("IPv4 prefix should be returned");
        let ipv6 = found
            .prefixes
            .iter()
            .find(|prefix| prefix.prefix.is_ipv6())
            .expect("IPv6 prefix should be returned");
        assert_eq!(ipv4.num_free_ips, Some(2));
        assert_eq!(ipv6.num_free_ips, Some(u64::MAX as u128 - 1));

        Ok(())
    }

    #[crate::sqlx_test]
    async fn for_prefix_containing_address_ignores_dhcpv6_link_address(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let link_address = "2001:db8:ffff::1";

        // A DHCPv6 link-address outside the prefix must not make the segment
        // own that address for static assignment.
        persist_test_segment(
            &pool,
            "link-only",
            "2001:db8:a::/64",
            None,
            Some(link_address),
        )
        .await?;
        let mut txn = pool.begin().await?;
        let segment = for_prefix_containing_address(&mut txn, link_address.parse()?).await?;
        assert!(segment.is_none());
        txn.rollback().await?;

        // If another segment's real prefix contains the same address, static
        // ownership follows the prefix, not the link-address equality branch.
        let owner_segment =
            persist_test_segment(&pool, "prefix-owner", "2001:db8:ffff::/64", None, None).await?;
        let mut txn = pool.begin().await?;
        let segment = for_prefix_containing_address(&mut txn, link_address.parse()?)
            .await?
            .expect("prefix-containing segment should resolve");
        assert_eq!(segment.id, owner_segment);
        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn for_relay_prefers_exact_dhcpv6_link_address_over_prefix_containment(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let relay = "2001:db8:ffff::1";

        // One segment matches only because its managed prefix contains the relay.
        persist_test_segment(&pool, "prefix-owner", "2001:db8:ffff::/64", None, None).await?;

        // The other segment owns the relay by exact DHCPv6 link-address.
        let exact_segment =
            persist_test_segment(&pool, "link-owner", "2001:db8:a::/64", None, Some(relay)).await?;

        // The exact link-address match should disambiguate the relay lookup.
        let mut txn = pool.begin().await?;
        let segment = for_relay(&mut txn, relay.parse()?)
            .await?
            .expect("relay should resolve to exact link-address segment");
        assert_eq!(segment.id, exact_segment);
        txn.rollback().await?;

        Ok(())
    }

    // A brand-new network is declared but no segment exists yet and no
    // snapshot has been recorded. reconcile_network_defs must return it in
    // the to-create list and leave all writes to the caller.
    #[crate::sqlx_test]
    async fn test_reconcile_network_defs_brand_new_is_returned(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let def = NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix: "192.168.1.0/24".parse().unwrap(),
            prefix_v6: None,
            gateway: "192.168.1.1".parse().unwrap(),
            dhcpv6_link_address: None,
            mtu: 1500,
            reserve_first: 5,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        };

        let mut txn = pool.begin().await?;
        let declared: HashMap<String, NetworkDefinition> = [("brand-new".to_string(), def.clone())]
            .into_iter()
            .collect();

        let to_create = reconcile_network_defs(&mut txn, &declared).await?;

        // reconcile must not have written a snapshot for a brand-new network.
        let stored = stored_def(txn.as_mut(), "brand-new").await?;
        assert!(
            stored.is_none(),
            "reconcile must not write a snapshot for a brand-new network; \
             that is the caller's responsibility"
        );

        // reconcile must not have created a network_segments row.
        assert!(
            !segment_exists(&mut txn, "brand-new").await?,
            "reconcile must not create a network_segments row for a brand-new network"
        );

        // reconcile must return the brand-new network so the caller can create it.
        assert_eq!(
            to_create,
            vec![("brand-new".to_string(), def)],
            "brand-new network must appear in the returned to-create list"
        );

        txn.rollback().await?;
        Ok(())
    }

    // Test-only constructor for a `NetworkDefinition` with sensible defaults
    fn def(prefix: &str, gateway: &str) -> NetworkDefinition {
        NetworkDefinition {
            segment_type: NetworkDefinitionSegmentType::Admin,
            prefix: prefix.parse().unwrap(),
            prefix_v6: None,
            gateway: gateway.parse().unwrap(),
            dhcpv6_link_address: None,
            mtu: 1500,
            reserve_first: 3,
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
            vpc_name: None,
        }
    }

    fn declared_one(name: &str, def: NetworkDefinition) -> HashMap<String, NetworkDefinition> {
        [(name.to_string(), def)].into_iter().collect()
    }

    // A segment row exists in `network_segments` but has no `network_def`
    // snapshot
    // Reconcile must record the snapshot without re-creating the segment.
    #[crate::sqlx_test]
    async fn reconcile_network_defs_backfills_existing_segment(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        minimum_segment_data(&pool, "pre-existing").await?;

        let mut txn = pool.begin().await?;
        let def = def("192.168.1.0/24", "192.168.1.1");

        let to_create =
            reconcile_network_defs(&mut txn, &declared_one("pre-existing", def.clone())).await?;
        assert!(
            to_create.is_empty(),
            "this arm must not add networks to the to-create list"
        );

        let stored = stored_def(txn.as_mut(), "pre-existing").await?;
        assert_eq!(stored.as_ref(), Some(&def), "snapshot must be backfilled");
        txn.rollback().await?;
        Ok(())
    }

    // Segment + snapshot both already exist and match the declaration
    #[crate::sqlx_test]
    async fn reconcile_network_defs_in_sync_is_noop(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let segment_id = minimum_segment_data(&pool, "stable").await?;

        let mut txn = pool.begin().await?;
        let def = def("192.168.1.0/24", "192.168.1.1");
        insert_network_def(&mut txn, "stable", segment_id, &def).await?;

        let to_create =
            reconcile_network_defs(&mut txn, &declared_one("stable", def.clone())).await?;
        assert!(
            to_create.is_empty(),
            "this arm must not add networks to the to-create list"
        );

        let stored = stored_def(txn.as_mut(), "stable").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&def),
            "in-sync snapshot must be left untouched",
        );
        txn.rollback().await?;
        Ok(())
    }

    // Segment + snapshot exist, but the declared definition has drifted
    // since seed. Reconcile must warn and leave the stored snapshot alone,
    // not silently reapply the new declaration.
    #[crate::sqlx_test]
    async fn reconcile_network_defs_drift_does_not_apply(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let segment_id = minimum_segment_data(&pool, "drifty").await?;

        let mut txn = pool.begin().await?;
        let original = def("192.168.1.0/24", "192.168.168.1");
        let drifted = def("10.0.0.0/24", "10.0.0.1");
        insert_network_def(&mut txn, "drifty", segment_id, &original).await?;

        let to_create =
            reconcile_network_defs(&mut txn, &declared_one("drifty", drifted.clone())).await?;
        assert!(
            to_create.is_empty(),
            "this arm must not add networks to the to-create list"
        );

        let stored = stored_def(txn.as_mut(), "drifty").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&original),
            "drift path must not overwrite the stored snapshot",
        );
        txn.rollback().await?;
        Ok(())
    }

    // A snapshot exists for a network that is no longer mentioned in any
    // declared config — typical of an operator removing the definition.
    // Reconcile must warn but not delete the snapshot
    #[crate::sqlx_test]
    async fn reconcile_network_defs_dropped_declaration_is_orphaned(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let segment_id = minimum_segment_data(&pool, "abandoned").await?;

        let mut txn = pool.begin().await?;
        let def = def("192.168.1.0/24", "192.168.1.1");
        insert_network_def(&mut txn, "abandoned", segment_id, &def).await?;

        let empty: HashMap<String, NetworkDefinition> = HashMap::new();
        let to_create = reconcile_network_defs(&mut txn, &empty).await?;
        assert!(
            to_create.is_empty(),
            "this arm must not add networks to the to-create list"
        );

        let stored = stored_def(txn.as_mut(), "abandoned").await?;
        assert_eq!(
            stored.as_ref(),
            Some(&def),
            "dropped declarations must not be deleted from network_def",
        );
        assert!(
            segment_exists(&mut txn, "abandoned").await?,
            "dropped declarations must not be deleted from network_segments",
        );
        txn.rollback().await?;
        Ok(())
    }

    // Deleting a segment via `final_delete` must cascade-delete its
    // `network_def` snapshot.
    #[crate::sqlx_test]
    async fn final_delete_cascades_to_network_def(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let segment_id = minimum_segment_data(&pool, "doomed").await?;

        // Seed the snapshot in its own committed transaction so it
        // survives into the next one where we run `final_delete`.
        let mut txn = pool.begin().await?;
        let def = def("192.168.1.0/24", "192.168.1.1");
        insert_network_def(&mut txn, "doomed", segment_id, &def).await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        final_delete(segment_id, &mut txn).await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        assert!(
            stored_def(txn.as_mut(), "doomed").await?.is_none(),
            "deleting a segment must cascade-delete its network_def snapshot",
        );
        assert!(
            !segment_exists(&mut txn, "doomed").await?,
            "segment row should be gone after final_delete",
        );
        Ok(())
    }

    /// `final_delete` must recheck the allocation predicate in its own
    /// transaction so a network configuration that retains only a prefix
    /// cannot disappear between the controller drain check and physical
    /// deletion.
    #[crate::sqlx_test]
    async fn final_delete_rejects_instance_network_config_reference(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let segment_id = minimum_segment_data(&pool, "still-referenced").await?;
        let machine_id = uuid::Uuid::new_v4();
        let mut txn = pool.begin().await?;
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn.as_mut())
            .await?;
        sqlx::query(
            "INSERT INTO instances (machine_id, network_config) \
             VALUES ($1, jsonb_build_object( \
                 'interfaces', jsonb_build_array( \
                     jsonb_build_object('network_segment_id', $2::text))))",
        )
        .bind(machine_id)
        .bind(segment_id)
        .execute(txn.as_mut())
        .await?;

        let error = final_delete(segment_id, &mut txn)
            .await
            .expect_err("referenced segment must remain present");
        assert!(matches!(
            error,
            DatabaseError::NetworkSegmentDelete(message)
                if message.contains("allocations still reference it")
        ));
        assert!(
            segment_exists(txn.as_mut(), "still-referenced").await?,
            "failed final deletion must retain the segment",
        );
        txn.rollback().await?;
        Ok(())
    }

    /// SVI allocation must lock the `instance_addresses` table before it
    /// updates any prefix row. Otherwise, a tenant segment whose prefixes
    /// reserve different numbers of addresses can deadlock with final deletion:
    /// allocation holds the first prefix row and waits for the table lock while
    /// deletion holds the table lock and waits for that prefix row.
    #[crate::sqlx_test]
    async fn tenant_svi_allocation_locks_before_mixed_prefix_updates(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut setup_txn = pool.begin().await?;
        let mut segment = persist(
            NewNetworkSegment {
                id: NetworkSegmentId::new(),
                name: "mixed-svi-lock-order".to_string(),
                subdomain_id: None,
                vpc_id: None,
                mtu: 1500,
                prefixes: vec![
                    NewNetworkPrefix {
                        prefix: "198.51.100.0/29".parse()?,
                        gateway: None,
                        dhcpv6_link_address: None,
                        num_reserved: 3,
                    },
                    NewNetworkPrefix {
                        prefix: "2001:db8:2403::/64".parse()?,
                        gateway: None,
                        dhcpv6_link_address: None,
                        num_reserved: 0,
                    },
                ],
                vlan_id: None,
                vni: None,
                segment_type: NetworkSegmentType::Tenant,
                can_stretch: Some(true),
                allocation_strategy: Default::default(),
                infer_slaac_eui64_addresses: false,
            },
            setup_txn.as_mut(),
            NetworkSegmentControllerState::Ready,
        )
        .await?;
        setup_txn.commit().await?;

        // `json_agg` does not promise prefix order. Force the dangerous order:
        // update the third address directly, then fall back to the allocator.
        segment
            .prefixes
            .sort_by_key(|prefix| std::cmp::Reverse(prefix.num_reserved));
        assert_eq!(segment.prefixes[0].num_reserved, 3);
        assert_eq!(segment.prefixes[1].num_reserved, 0);

        let mut deletion_txn = pool.begin().await?;
        let deletion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(deletion_txn.as_mut())
            .await?;
        sqlx::query("LOCK TABLE instance_addresses IN ACCESS SHARE MODE")
            .execute(deletion_txn.as_mut())
            .await?;

        let allocation_pool = pool.clone();
        let segment_id = segment.id;
        let allocation_task = tokio::spawn(async move {
            let mut txn = allocation_pool.begin().await.unwrap();
            let result = allocate_svi_ip(&segment, &mut txn).await;
            if result.is_ok() {
                txn.commit().await.unwrap();
            } else {
                txn.rollback().await.unwrap();
            }
            result
        });

        // Wait until allocation reaches the table boundary held above. With
        // the correct order it has not touched either prefix row yet.
        let mut allocation_is_blocked = false;
        for _ in 0..300 {
            allocation_is_blocked = sqlx::query_scalar(
                r#"SELECT EXISTS (
                    SELECT 1
                    FROM pg_stat_activity AS activity
                    WHERE activity.datname = current_database()
                      AND activity.wait_event_type = 'Lock'
                      AND $1 = ANY(pg_blocking_pids(activity.pid))
                      AND activity.query ILIKE
                          '%LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE%'
                )"#,
            )
            .bind(deletion_pid)
            .fetch_one(&pool)
            .await?;
            if allocation_is_blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            allocation_is_blocked,
            "SVI allocation never reached the lock on instance_addresses",
        );

        // Acquiring the table lock before the prefix lock lets this delete
        // finish. Roll it back so the waiting allocator can resume against the
        // restored rows.
        assert_eq!(
            final_delete(segment_id, &mut deletion_txn).await?,
            segment_id,
        );
        deletion_txn.rollback().await?;

        allocation_task
            .await
            .expect("SVI allocation task must complete")
            .expect("SVI allocation must resume after deletion rollback");
        Ok(())
    }

    // `network_segments.name` is not UNIQUE at the DB layer, so two rows
    // can share a name Reconcile's backfill
    // path can't tell which one to attach a snapshot to, so it must
    // skip rather than guess.
    #[crate::sqlx_test]
    async fn reconcile_network_defs_skips_backfill_on_duplicate_names(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Two segments, same name, neither has a snapshot.
        minimum_segment_data(&pool, "ambiguous").await?;
        minimum_segment_data(&pool, "ambiguous").await?;

        let mut txn = pool.begin().await?;
        let def = def("192.168.1.0/24", "192.168.1.1");

        let to_create =
            reconcile_network_defs(&mut txn, &declared_one("ambiguous", def.clone())).await?;
        assert!(
            to_create.is_empty(),
            "this arm must not add networks to the to-create list"
        );

        assert!(
            stored_def(txn.as_mut(), "ambiguous").await?.is_none(),
            "backfill must skip when multiple segments share the declared name",
        );
        txn.rollback().await?;
        Ok(())
    }
}

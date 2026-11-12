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
//!
//! Machine - represents a database-backed Machine object
//!
//! Network configuration transaction boundaries are documented on
//! [`try_update_network_config`] and [`update_dpu_loopback_ips_v6`]. The
//! crate's `LOCK_ORDERING.md` traces their cross-crate callers.

use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{IpAddr, Ipv6Addr};
use std::ops::Deref;
use std::str::FromStr;

use carbide_uuid::dpa_interface::DpaInterfaceId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::instance_type::InstanceTypeId;
use carbide_uuid::machine::{
    DpuMachineId, HostMachineId, MachineId, MachineIdSubtypeTrait, MachineInterfaceId, MachineType,
    PredictedHostMachineId, StableHostMachineId,
};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::rack::{RackId, RackProfileId};
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use health_report::{HealthReport, HealthReportApplyMode};
use itertools::Itertools;
use lazy_static::lazy_static;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::expected_machine::ExpectedMachineData;
use model::extension_service::ExtensionServiceType;
use model::hardware_info::{
    MachineInventory, MachineNvLinkInfo, mnnvl_gpu_name_sql_like_conditions,
};
use model::instance::status::extension_service::InstanceExtensionServiceStatusObservation;
use model::machine::infiniband::MachineInfinibandStatusObservation;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::network::{
    DpuFabricInterfaceStatusObservation, MachineNetworkStatusObservation, ManagedHostNetworkConfig,
    ManagedHostQuarantineState,
};
use model::machine::nic_firmware::{NicFirmwareOperation, NicFirmwarePurpose};
use model::machine::nvlink::MachineNvLinkStatusObservation;
use model::machine::spx::MachineSpxStatusObservation;
use model::machine::spx_multiplane::{MachineSpxMultiplane, MachineSpxMultiplaneStatus};
use model::machine::status::MlxDeviceObservation;
use model::machine::upgrade_policy::AgentUpgradePolicy;
use model::machine::{
    AnyMachine, CURRENT_STATE_MODEL_VERSION, Dpf, DpuAgentUpgradeStatus, DpuInfo,
    DpuInfoStatusObservation, DpuMachine, DpuOsOperationalState, DpuRepresentorStatus,
    FailureDetails, HostMachine, HostProfile, Machine, MachineInterfaceSnapshot,
    MachineLastRebootRequested, MachineLastRebootRequestedMode, MachineMaintenanceOperation,
    MachineValidationContext, ManagedHostState, ReprovisionRequest, ResetRequest, UpgradeDecision,
};
use model::machine_boot_interface::InterfaceSelectionSource;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::metadata::Metadata;
use model::nic_firmware::NicFirmwareProfileId;
use model::resource_pool;
use model::resource_pool::ResourcePoolError;
use model::resource_pool::common::{CommonPools, LOOPBACK_IP_V6};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, PgTransaction, Pool, Postgres, Row};

use super::{DatabaseError, ObjectFilter, Transaction, queries};
use crate::db_read::DbReader;
use crate::{ConditionalWrite, ControllerStateNotCurrent, DatabaseResult};

mod creation_identity;
pub use creation_identity::MachineCreationIdentity;

#[derive(Serialize)]
struct ReprovisionRequestRestart {
    pub update_firmware: bool,
    pub restart_reprovision_requested_at: DateTime<Utc>,
}

lazy_static! {
    pub static ref JSON_MACHINE_SNAPSHOT_WITH_HISTORY_QUERY: String = format!(
        "SELECT row_to_json(m.*) FROM ({}) m INNER JOIN machines ON machines.id = m.id",
        queries::MACHINE_SNAPSHOTS_WITH_HISTORY.as_str(),
    );
    /// Filters appended to this query must constrain `m.id` directly or through an id
    /// array such as `m.id = ANY(ARRAY(SELECT ...))`. A filter joined through another
    /// table is planned after the fleet-wide enrichment once the FROM list exceeds
    /// `join_collapse_limit`.
    pub static ref JSON_MACHINE_SNAPSHOT_QUERY: String = format!(
        "SELECT row_to_json(m.*) FROM ({}) m INNER JOIN machines ON machines.id = m.id",
        queries::MACHINE_SNAPSHOTS_NO_HISTORY.as_str(),
    );
}

/// An item associated with a row in `machines`.
pub trait MachineRowLockItem {
    /// Returns the ID used to order machine-row lock acquisition.
    fn machine_id(&self) -> MachineId;
}

impl MachineRowLockItem for MachineId {
    fn machine_id(&self) -> MachineId {
        *self
    }
}

impl<T> MachineRowLockItem for (MachineId, T) {
    fn machine_id(&self) -> MachineId {
        self.0
    }
}

/// An owning iterator in canonical `machines` row-lock acquisition order.
///
/// PostgreSQL holds row locks acquired by `UPDATE` until the transaction ends.
/// If two transactions update overlapping machine batches in different orders,
/// each can hold a row needed by the other, and PostgreSQL aborts one with
/// SQLSTATE 40P01 (`deadlock_detected`). A common order prevents that cycle.
///
/// Construct this before the batch's first machine-row write so concurrent
/// transactions acquire row locks in the same order.
pub struct MachineRowLockOrderIter<T>(std::vec::IntoIter<T>);

impl<T: MachineRowLockItem> MachineRowLockOrderIter<T> {
    /// Orders machine-scoped batch items by their machine IDs.
    pub fn new(mut items: Vec<T>) -> Self {
        items.sort_unstable_by_key(MachineRowLockItem::machine_id);
        Self(items.into_iter())
    }
}

impl<T> MachineRowLockOrderIter<T> {
    /// Returns the remaining ordered items as a slice.
    pub fn as_slice(&self) -> &[T] {
        self.0.as_slice()
    }
}

impl<T> Iterator for MachineRowLockOrderIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<T> ExactSizeIterator for MachineRowLockOrderIter<T> {}

/// `get_or_create` returns the machine matching an interface, creating it if absent.
/// `common_pools` selects creation allocations; `None` skips allocation.
/// For a new DPU with pools, `requested_loopback_v6` claims that exact address;
/// omitting it permits optional IPv6 auto-assignment. Existing rows retain their
/// allocations. The caller holds any IPv6 reservation lock through creation.
pub async fn get_or_create(
    txn: &mut PgConnection,
    common_pools: Option<MachineCreationPools<'_>>,
    stable_machine_id: &MachineId,
    interface: &MachineInterfaceSnapshot,
    requested_loopback_v6: Option<Ipv6Addr>,
) -> DatabaseResult<AnyMachine> {
    let existing_machine =
        find_one(&mut *txn, stable_machine_id, MachineSearchConfig::default()).await?;
    if let Some(machine_id) = interface.machine_id.as_ref() {
        if machine_id != stable_machine_id {
            return Err(DatabaseError::internal(format!(
                "Database inconsistency: MachineId {} on interface {} does not match stable machine ID {} which now uses this interface",
                machine_id, interface.id, stable_machine_id
            )));
        }

        if existing_machine.is_none() {
            tracing::warn!(
                %machine_id,
                machine_interface_id = %interface.id,
                "Interface ID refers to missing machine",
            );
            return Err(DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            });
        }
    }

    // Get or create
    if let Some(machine) = existing_machine {
        // New site-explorer redfish discovery path.
        crate::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Machine(machine.id),
            txn,
        )
        .await?;
        // Attribute only a newly adopted primary so re-registration cannot overwrite an owned interface's selection source.
        if interface.machine_id.is_none()
            && stable_machine_id.machine_type().is_host()
            && crate::machine_interface::find_one(&mut *txn, interface.id)
                .await?
                .primary_interface
        {
            crate::machine_interface::set_primary_interface_selection(
                txn,
                stable_machine_id,
                InterfaceSelectionSource::LegacyUnknown,
                true,
            )
            .await?;
        }
        Ok(machine)
    } else {
        // Old manual discovery path.
        // Host and DPU machines are created in same `discover_machine` call. Update same
        // state in both machines.
        let state = ManagedHostState::Created;
        let machine = create_with_requested_loopback(
            txn,
            common_pools,
            stable_machine_id,
            state,
            None,
            CURRENT_STATE_MODEL_VERSION,
            requested_loopback_v6,
        )
        .await?;
        crate::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Machine(machine.id),
            txn,
        )
        .await?;
        if stable_machine_id.machine_type().is_host() && interface.primary_interface {
            crate::machine_interface::set_primary_interface_selection(
                txn,
                stable_machine_id,
                InterfaceSelectionSource::LegacyUnknown,
                true,
            )
            .await?;
        }
        Ok(machine)
    }
}

pub async fn find_one<ID>(
    txn: impl DbReader<'_>,
    id: &ID,
    search_config: MachineSearchConfig,
) -> Result<Option<Machine<ID>>, DatabaseError>
where
    ID: MachineIdSubtypeTrait,
    ID: TryFrom<MachineId>,
    DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    Ok(find(txn, ObjectFilter::One(*id), search_config)
        .await?
        .pop())
}

/// `find_dpu_id` returns the requested DPU's ID, or `None` if its row is absent.
/// Other snapshot fields are neither read nor decoded.
pub async fn find_dpu_id(
    conn: impl DbReader<'_>,
    id: &DpuMachineId,
) -> DatabaseResult<Option<DpuMachineId>> {
    let query = "SELECT id FROM machines WHERE id = $1";
    sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `DpuNetworkStatusInputs` contains the persisted inputs needed to apply a DPU status report.
#[derive(Debug)]
pub struct DpuNetworkStatusInputs {
    /// Desired network configuration and its version, including a pending change to the admin network.
    pub network_config: Versioned<ManagedHostNetworkConfig>,
    /// Previous network observation, or `None` if no observation is stored.
    pub network_status_observation: Option<MachineNetworkStatusObservation>,
    /// Previous DPU agent merge report; replacement reports do not supply alert timestamps.
    pub dpu_agent_health_report: Option<HealthReport>,
}

/// Reads the inputs for a DPU status report without snapshot enrichment or row locks.
/// Returns `None` for an absent machine and an error if selected fields cannot be decoded.
pub async fn find_dpu_network_status_inputs(
    conn: impl DbReader<'_>,
    dpu_machine_id: &DpuMachineId,
) -> DatabaseResult<Option<DpuNetworkStatusInputs>> {
    #[derive(FromRow)]
    struct QueryResult {
        network_config: sqlx::types::Json<ManagedHostNetworkConfig>,
        network_config_version: ConfigVersion,
        network_status_observation: sqlx::types::Json<Option<MachineNetworkStatusObservation>>,
        health_reports: sqlx::types::Json<Option<model::health::HealthReportSources>>,
    }

    // SQL NULL and JSON null both mean no observation or health reports.
    let query = "SELECT network_config, network_config_version,
                        COALESCE(network_status_observation, 'null'::jsonb)
                            AS network_status_observation,
                        COALESCE(health_reports, 'null'::jsonb) AS health_reports
                 FROM machines WHERE id = $1";
    let row: Option<QueryResult> = sqlx::query_as(query)
        .bind(dpu_machine_id)
        .fetch_optional(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    Ok(row.map(|row| DpuNetworkStatusInputs {
        network_config: Versioned::new(row.network_config.0, row.network_config_version),
        network_status_observation: row.network_status_observation.0,
        dpu_agent_health_report: row
            .health_reports
            .0
            .and_then(|mut reports| reports.merges.remove(HealthReport::DPU_AGENT_SOURCE)),
    }))
}

pub(crate) async fn find_by_nic_firmware_profile(
    db: impl DbReader<'_>,
    profile_id: &NicFirmwareProfileId,
) -> DatabaseResult<Vec<HostMachine>> {
    let query = format!(
        "{} WHERE m.id = ANY(ARRAY(\
            SELECT id FROM machines WHERE nic_firmware_profile_id = $1 \
            UNION \
            SELECT machine_id FROM machine_nic_firmware_overrides WHERE profile_id = $1\
        ))",
        JSON_MACHINE_SNAPSHOT_QUERY.as_str()
    );
    let machines: Vec<AnyMachine> = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(profile_id)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(&query, error))?;
    machines
        .into_iter()
        .map(|machine| machine.try_into_subtype().map_err(Into::into))
        .collect()
}

/// Read the current database clock, rather than the transaction's start time.
pub async fn database_now(db: impl DbReader<'_>) -> Result<DateTime<Utc>, DatabaseError> {
    let query = "SELECT clock_timestamp()";
    sqlx::query_scalar(query)
        .fetch_one(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// Lock the machine row before reading its mutable request state. A separate
/// read after this query sees changes committed by the previous lock holder.
pub async fn lock_by_id_for_update(
    txn: &mut PgConnection,
    id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "SELECT id FROM machines WHERE id = $1 FOR UPDATE";
    sqlx::query_scalar::<_, MachineId>(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// `try_lock_group_for_update` tries a host and its DPUs in machine-row order.
/// Some network writers already hold the host, so waiting here while holding
/// a DPU could deadlock with their group version update.
///
/// Returns false if a member is missing or busy. Both outcomes retain any
/// acquired locks; on false, roll back the whole transaction before retrying.
/// Finish interface writes first and retain successful locks through commit.
pub async fn try_lock_group_for_update(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT id FROM machine_group_member_ids($1)";
    let machine_ids: Vec<MachineId> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if machine_ids.is_empty() {
        return Ok(false);
    }
    for member_id in MachineRowLockOrderIter::new(machine_ids) {
        if !try_lock_by_id_for_update(txn, &member_id).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Locks a machine without waiting for another writer. Returns false when the
/// row is absent or locked; the caller must release any locks it already holds.
pub async fn try_lock_by_id_for_update(
    txn: &mut PgConnection,
    id: &MachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT id FROM machines WHERE id = $1 FOR UPDATE SKIP LOCKED";
    sqlx::query_scalar::<_, MachineId>(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map(|id| id.is_some())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Tries a machine's non-key update lock without waiting for another writer.
/// Compatible foreign-key KEY SHARE locks are allowed. Returns false for a
/// missing or busy row; earlier locks remain held until the transaction ends.
/// Roll back the whole rejected attempt before waiting or retrying.
pub async fn try_lock_by_id_for_no_key_update(
    txn: &mut PgConnection,
    id: &MachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE SKIP LOCKED";
    sqlx::query_scalar::<_, MachineId>(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map(|id| id.is_some())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Tries a machine's state-history retention advisory without changing history.
/// Success retains the advisory until transaction end. A false result takes no
/// new advisory; roll back earlier work before waiting or retrying the attempt.
pub async fn try_lock_machine_state_history(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT pg_try_advisory_xact_lock(\
                     hashtextextended($1, 'machine_state_history'::regclass::bigint)\
                 )";
    sqlx::query_scalar(query)
        .bind(machine_id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn find_existing_machine(
    txn: &mut PgConnection,
    macaddr: MacAddress,
    relay: IpAddr,
) -> Result<Option<MachineId>, DatabaseError> {
    // Exact DHCPv6 link-address matches are authoritative. Prefix containment
    // is only valid when no segment claims the relay by exact link-address.
    let query = "
    -- Start from machines already associated with the observed MAC.
    SELECT m.id FROM
    machines m
    INNER JOIN machine_interfaces mi
        ON m.id = mi.machine_id
    INNER JOIN network_segments ns
        ON mi.segment_id = ns.id
    WHERE
        mi.mac_address = $1::macaddr
        AND
        -- BMC interfaces are not host identity for DHCP discovery.
        mi.interface_type != 'Bmc'
        AND
        -- Keep segments that own the relay address.
        EXISTS (
            SELECT 1
            FROM network_prefixes np
            WHERE np.segment_id = ns.id
            AND (
                -- Accept this candidate segment when one of its own prefixes
                -- has the exact DHCPv6 relay link-address.
                np.dhcpv6_link_address = $2::inet
                OR (
                    -- This check is intentionally global, not redundant:
                    -- prefix fallback is allowed only when no segment anywhere
                    -- has an exact DHCPv6 link-address claim for this relay.
                    NOT EXISTS (
                        SELECT 1
                        FROM network_prefixes exact_np
                        WHERE exact_np.dhcpv6_link_address = $2::inet
                    )
                    -- With no exact claim, IPv4/v6 prefix containment can match
                    -- the segment that owns the relay address.
                    AND $2::inet <<= np.prefix
                )
            )
        )";

    let id: Option<MachineId> = sqlx::query_as(query)
        .bind(macaddr)
        .bind(relay)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(id)
}

/// Perform an arbitrary action to a Machine and advance it to the next state given the last
/// state.
///
/// Arguments:
///
/// * `txn` - A reference to a currently open database transaction
/// * `state` - A reference to a MachineState enum
// TODO: abhi, Make it private.
pub async fn advance<ID: MachineIdSubtypeTrait>(
    machine: &Machine<ID>,
    txn: &mut PgConnection,
    state: &ManagedHostState,
    version: Option<ConfigVersion>,
) -> Result<bool, DatabaseError> {
    // Get current version
    let version = version.unwrap_or_else(|| machine.state.version.increment());

    // Store history of machine state changes.
    crate::state_history::persist(
        txn,
        crate::state_history::StateHistoryTableId::Machine,
        &machine.id,
        state,
        version,
    )
    .await?;

    let _id: (String,) = sqlx::query_as(
            "UPDATE machines SET controller_state_version=$1, controller_state=$2 WHERE id=$3 RETURNING id",
        )
        .bind(version)
        .bind(sqlx::types::Json(state))
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update machines state", e))?;

    Ok(true)
}

/// Find machines given a set of criteria
///
/// Arguments:
///
/// * `txn`           - A reference to a currently open database transaction
/// * `filter`        - An ObjectFilter to control the size of the response set
/// * `search_config` - A MachineSearchConfig with search options to control the
///   records selected
pub async fn find<ID>(
    txn: impl DbReader<'_>,
    filter: ObjectFilter<'_, ID>,
    search_config: MachineSearchConfig,
) -> Result<Vec<Machine<ID>>, DatabaseError>
where
    ID: MachineIdSubtypeTrait,
    ID: TryFrom<MachineId>,
    DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    // The TRUE will be optimized away by the query planner,
    // but it simplifies the rest of the building for us.
    lazy_static! {
        static ref query_no_history: String =
            format!("{} WHERE TRUE", JSON_MACHINE_SNAPSHOT_QUERY.deref());
        static ref query_with_history: String = format!(
            "{} WHERE TRUE",
            JSON_MACHINE_SNAPSHOT_WITH_HISTORY_QUERY.deref()
        );
    }

    let mut builder = sqlx::QueryBuilder::new(if search_config.include_history {
        query_with_history.deref()
    } else {
        query_no_history.deref()
    });

    match filter {
        ObjectFilter::All => {} // Nothing to add.
        ObjectFilter::One(id) => {
            builder.push(" AND m.id= ");
            builder.push_bind(id.to_string());
        }
        ObjectFilter::List(list) => {
            builder.push(" AND m.id=ANY( ");
            builder.push_bind(
                list.iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<String>>(),
            );
            builder.push(" ) ");
        }
    }

    if search_config.only_maintenance {
        builder.push(
            " AND m.health_reports->'merges'->'maintenance'->'alerts'->0->>'id' = 'Maintenance' ",
        );
    }

    if search_config.only_quarantine {
        builder.push(" AND m.network_config->>'quarantine_state' IS NOT NULL ");
    }

    if let Some(id) = search_config.instance_type_id {
        builder.push(" AND m.instance_type_id = ");
        builder.push_bind(id);
    }

    if let Some(rack_id) = search_config.rack_id {
        builder.push(" AND m.rack_id = ");
        builder.push_bind(rack_id);
    }

    if search_config.for_update {
        // Match `MachineId::cmp` independently of the database locale.
        builder.push(" ORDER BY m.id COLLATE \"C\" ");
        builder.push(" FOR UPDATE OF machines ");
    };

    let all_machines: Vec<AnyMachine> = builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))?;

    Ok(all_machines
        .into_iter()
        .map(AnyMachine::try_into_subtype)
        .collect::<Result<Vec<Machine<_>>, _>>()?)
}

pub async fn find_by_ip(
    txn: &mut PgConnection,
    ip: &IpAddr,
) -> Result<Option<AnyMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{}
                WHERE m.id = ANY(ARRAY(
                    SELECT mi.machine_id FROM machine_interfaces mi
                    INNER JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
                    WHERE mia.address = $1::inet
                    AND mi.interface_type != 'Bmc'
                    AND mi.machine_id IS NOT NULL))"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(ip.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn find_id_by_bmc_ip(
    txn: &mut PgConnection,
    bmc_ip: &IpAddr,
) -> Result<Option<MachineId>, DatabaseError> {
    crate::machine_topology::find_machine_id_by_bmc_ip(txn, &bmc_ip.to_string()).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteExplorerMachineAuditState {
    pub bmc_ip: IpAddr,
    pub machine_id: MachineId,
    pub site_explorer_health_report: Option<HealthReport>,
}

#[derive(Debug, sqlx::FromRow)]
struct SiteExplorerMachineAuditStateRow {
    bmc_ip: IpAddr,
    machine_id: MachineId,
    site_explorer_health_report: Option<sqlx::types::Json<HealthReport>>,
}

pub async fn find_site_explorer_machine_audit_states_by_bmc_ips(
    txn: impl DbReader<'_>,
    bmc_ips: &[IpAddr],
) -> Result<Vec<SiteExplorerMachineAuditState>, DatabaseError> {
    if bmc_ips.is_empty() {
        return Ok(Vec::new());
    }

    let query = r#"
        SELECT
            mia.address AS bmc_ip,
            mi.machine_id,
            m.health_reports->'merges'->'site-explorer' AS site_explorer_health_report
        FROM machine_interfaces mi
        JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
        JOIN machines m ON m.id = mi.machine_id
        WHERE mi.interface_type = 'Bmc'
            AND mi.machine_id IS NOT NULL
            AND mia.address = ANY($1::inet[])
    "#;
    let bmc_ip_networks: Vec<ipnetwork::IpNetwork> =
        bmc_ips.iter().map(|bmc_ip| (*bmc_ip).into()).collect();
    let rows: Vec<SiteExplorerMachineAuditStateRow> = sqlx::query_as(query)
        .bind(bmc_ip_networks)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(rows
        .into_iter()
        .map(|row| SiteExplorerMachineAuditState {
            bmc_ip: row.bmc_ip,
            machine_id: row.machine_id,
            site_explorer_health_report: row.site_explorer_health_report.map(|report| report.0),
        })
        .collect())
}

/// Finds machines associated with a specified instance type
///
/// * `txn`              - A reference to an active DB transaction
/// * `instance_type_id` - An reference to an InstanceTypeId to query for
/// * `for_update`       - A boolean flag to acquire DB locks for synchronization
pub async fn find_ids_by_instance_type_id(
    txn: &mut PgConnection,
    instance_type_id: &InstanceTypeId,
    for_update: bool,
) -> Result<Vec<(HostMachineId, ConfigVersion)>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id, version FROM machines WHERE");

    builder.push(" instance_type_id = ");
    builder.push_bind(instance_type_id);

    if for_update {
        builder.push(" ORDER BY id ");
        builder.push(" FOR UPDATE ");
    }

    builder
        .build_query_as()
        .bind(instance_type_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Finds NVLink info for a list of machine IDs
///
/// * `txn` - A reference to an active DB transaction
/// * `machine_ids` - A slice of machine IDs to query for
pub async fn find_nvlink_info_by_machine_ids(
    txn: &mut PgConnection,
    machine_ids: &[HostMachineId],
) -> Result<HashMap<HostMachineId, Option<MachineNvLinkInfo>>, DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let query = "SELECT id, nvlink_info FROM machines WHERE id = ANY($1)";
    let machine_id_strings: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();

    let rows = sqlx::query(query)
        .bind(machine_id_strings)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    let mut result = HashMap::new();
    for row in rows {
        let machine_id: HostMachineId =
            row.try_get(0).map_err(|e| DatabaseError::query(query, e))?;
        let nvlink_info: Option<sqlx::types::Json<MachineNvLinkInfo>> =
            row.try_get(1).map_err(|e| DatabaseError::query(query, e))?;
        let nvlink_info = nvlink_info.map(|json| json.0);
        result.insert(machine_id, nvlink_info);
    }

    Ok(result)
}

async fn update_machine_instance_type(
    txn: &mut PgConnection,
    instance_type_id: Option<&InstanceTypeId>,
    machine_versions: &[(&HostMachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    if machine_versions.is_empty() {
        return Ok(vec![]);
    }
    let mut builder = sqlx::QueryBuilder::default();
    builder
        .push("UPDATE machines AS t SET instance_type_id = ")
        .push_bind(instance_type_id)
        .push("::varchar")
        .push(
            ", version = v.new_version \
               FROM ( ",
        )
        .push_values(machine_versions.iter(), |mut b, (id, old_version)| {
            let new_version = old_version.increment();
            b.push_bind(id)
                .push_bind(old_version)
                .push_bind(new_version);
        })
        .push(
            ") AS v(id, old_version, new_version) \
         WHERE t.id = v.id \
           AND t.version = v.old_version \
         RETURNING t.id",
        );
    builder
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(builder.sql(), e))
}

/// Associates machines with an InstanceType.
///
/// * `txn`              - A reference to an active DB transaction
/// * `instance_type_id` - An reference to an InstanceTypeId to associate with a set of machines
/// * `machine_ids`      - A list of machine IDs to associate to the desired instance type
pub async fn associate_machines_with_instance_type(
    txn: &mut PgConnection,
    instance_type_id: &InstanceTypeId,
    machine_versions: &[(&HostMachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    update_machine_instance_type(txn, Some(instance_type_id), machine_versions).await
}

/// Removes multiple machine associations with an InstanceType.
/// This does *NOT* check if the machines are in use.
///
/// * `txn`         - A reference to an active DB transaction
/// * `machine_ids` - A slice of machine IDs to update
pub async fn remove_instance_type_associations(
    txn: &mut PgConnection,
    machine_versions: &[(&HostMachineId, &ConfigVersion)],
) -> Result<Vec<MachineId>, DatabaseError> {
    update_machine_instance_type(txn, None, machine_versions).await
}

pub async fn find_by_hostname(
    txn: &mut PgConnection,
    hostname: &str,
) -> Result<Option<AnyMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.id = ANY(ARRAY(SELECT mi.machine_id FROM machine_interfaces mi WHERE mi.hostname = $1 AND mi.interface_type != 'Bmc' AND mi.machine_id IS NOT NULL))",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }

    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(hostname)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

pub async fn find_by_mac_address(
    txn: &mut PgConnection,
    mac_address: &MacAddress,
) -> Result<Option<AnyMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.id = ANY(ARRAY(SELECT mi.machine_id FROM machine_interfaces mi WHERE mi.mac_address = $1::macaddr AND mi.interface_type != 'Bmc' AND mi.machine_id IS NOT NULL))",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(mac_address)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

/// Returns the machine IDs owning the requested non-BMC interface MACs.
///
/// Unowned interfaces and missing MACs are omitted. Predictions are not consulted.
/// As with [`find_by_mac_address`], the owner chosen for a MAC associated with
/// multiple machines is unspecified.
pub async fn find_machine_ids_by_mac_addresses(
    db: impl DbReader<'_>,
    mac_addresses: &[MacAddress],
) -> DatabaseResult<HashMap<MacAddress, MachineId>> {
    if mac_addresses.is_empty() {
        return Ok(HashMap::new());
    }
    let query = r#"
        SELECT mi.mac_address, m.id
        FROM machine_interfaces mi
        JOIN machines m ON m.id = mi.machine_id
        WHERE mi.mac_address = ANY($1) AND mi.interface_type != 'Bmc'
    "#;
    let rows: Vec<(MacAddress, MachineId)> = sqlx::query_as(query)
        .bind(mac_addresses)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(rows.into_iter().collect())
}

/// Finds the machine with this address in either loopback field.
/// Both fields are serialized from typed IP addresses, so their stored text
/// matches `IpAddr::to_string()`.
/// Attached remote IPv4 ownership also identifies the machine while its
/// configuration field needs repair. Pending provider reservations do not.
pub async fn find_by_loopback_ip(
    txn: impl DbReader<'_>,
    loopback_ip: IpAddr,
) -> Result<Option<AnyMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.network_config->>'loopback_ip' = $1 OR m.network_config->>'loopback_ip_v6' = $1
                OR m.id IN (
                    SELECT owner_id FROM resource_pool_allocations
                    WHERE pool_name = 'lo-ip' AND owner_type = 'machine'
                      AND binding_id IS NOT NULL AND state = 'attached'
                      AND result->>'status' = 'allocated'
                      AND result->'result'->'value'->>'kind' = 'ip_address'
                      AND result->'result'->'value'->>'value' = $1
                )",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(loopback_ip.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;
    Ok(machine)
}

/// Finds a machine by a query
///
/// - If the query looks like a MachineId, it will try to load the information based on the MachineId
/// - If the query looks like an IP address, it will try to look up the machine based on its admin IP address
/// - If the query looks like a MAC address, it will look up the machine by MAC address
/// - Otherwise, it will try to look up the Machine by hostname
pub async fn find_by_query(
    txn: &mut PgConnection,
    query: &str,
) -> Result<Option<AnyMachine>, DatabaseError> {
    if let Ok(id) = MachineId::from_str(query) {
        return find_one(txn, &id, MachineSearchConfig::default()).await;
    }

    if let Ok(ip) = IpAddr::from_str(query) {
        return find_by_ip(txn, &ip).await;
    }

    if let Ok(mac) = MacAddress::from_str(query) {
        return find_by_mac_address(txn, &mac).await;
    }

    find_by_hostname(txn, query).await
}

/// Records the database statement execution time as the machine's reboot timestamp.
///
/// This ensures a reboot reported after a concurrent state transition is recorded as newer than
/// that transition, even when the reporting transaction began first.
pub async fn update_reboot_time<ID: MachineIdSubtypeTrait>(
    machine: &Machine<ID>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_reboot_time=clock_timestamp() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Records a new reboot or power request at the supplied time.
/// Reboots start unverified; other modes skip restart verification. The
/// verification attempt count starts at zero in either case.
pub async fn update_reboot_requested_explicit_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    mode: MachineLastRebootRequestedMode,
    time: DateTime<Utc>,
) -> Result<(), DatabaseError> {
    let mut restart_verified = None;
    if matches!(mode, MachineLastRebootRequestedMode::Reboot) {
        restart_verified = Some(false);
    }
    let data = MachineLastRebootRequested {
        time,
        mode,
        restart_verified,
        verification_attempts: Some(0),
    };

    record_reboot_request(machine_id, txn, &data).await
}

/// Records a new reboot or power request, replacing the previous record in full.
/// The caller supplies its timestamp, mode, and initial verification state.
/// Use `update_restart_verification_status` to verify an existing attempt.
pub async fn record_reboot_request(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    request: &MachineLastRebootRequested,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_reboot_requested=$1 WHERE id=$2 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(request))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_reboot_requested_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    mode: MachineLastRebootRequestedMode,
) -> Result<(), DatabaseError> {
    update_reboot_requested_explicit_time(machine_id, txn, mode, Utc::now()).await
}

/// The captured reboot record no longer matches, or the machine or request is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebootVerificationNotCurrent;

/// Updates verification only while every field of the captured request still matches.
///
/// Compares the record under a machine-row lock held until the caller's
/// transaction ends. Only verification status and attempt count are changed.
/// Missing machines, absent requests, and changed records share
/// `NotApplied(RebootVerificationNotCurrent)`.
pub async fn update_restart_verification_status(
    machine_id: &MachineId,
    current_reboot: MachineLastRebootRequested,
    verified: Option<bool>,
    attempts: i32,
    txn: &mut PgTransaction<'_>,
) -> Result<ConditionalWrite<(), RebootVerificationNotCurrent>, DatabaseError> {
    let query = "SELECT last_reboot_requested FROM machines WHERE id=$1 FOR NO KEY UPDATE";
    let stored: Option<Option<sqlx::types::Json<MachineLastRebootRequested>>> =
        sqlx::query_scalar(query)
            .bind(machine_id)
            .fetch_optional(txn.as_mut())
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    let Some(sqlx::types::Json(stored_reboot)) = stored.flatten() else {
        return Ok(ConditionalWrite::NotApplied(RebootVerificationNotCurrent));
    };

    // Compare typed values: legacy JSON can spell UTC as `+00:00` and omit
    // optional fields. A SQL timestamp cast would also lose nanoseconds.
    if stored_reboot != current_reboot {
        return Ok(ConditionalWrite::NotApplied(RebootVerificationNotCurrent));
    }

    let query = "UPDATE machines
                 SET last_reboot_requested = last_reboot_requested || jsonb_build_object(
                     'restart_verified', $1::boolean, 'verification_attempts', $2::integer)
                 WHERE id=$3 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(verified)
        .bind(attempts)
        .bind(machine_id)
        .fetch_one(txn.as_mut())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(ConditionalWrite::Applied(()))
}

/// Records the database statement execution time as the machine's cleanup timestamp.
///
/// This ensures cleanup completed after a concurrent state transition is recorded as newer than
/// that transition, even when the cleanup transaction began first.
pub async fn update_cleanup_time<ID: MachineIdSubtypeTrait>(
    machine: &Machine<ID>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    // A cleanup transaction can begin before a concurrent state transition commits. Record the
    // update time, not the transaction start time, so completed cleanup is newer than that state.
    let query = "UPDATE machines SET last_cleanup_time=clock_timestamp() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine.id.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn set_cleanup_time(
    machine_id: &MachineId,
    cleanup_time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_cleanup_time=$1 WHERE id=$2 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(cleanup_time)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn clear_cleanup_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_cleanup_time=NULL WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_bios_password_set_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET bios_password_set_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn clear_bios_password_set_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET bios_password_set_time=NULL WHERE id=$1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|e| DatabaseError::query(query, e))
}

/// Records the database statement execution time as the machine's discovery timestamp.
///
/// This ensures discovery completed after a concurrent state transition is recorded as newer than
/// that transition, even when the discovery transaction began first.
pub async fn update_discovery_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET last_discovery_time=clock_timestamp() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_scout_contact_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_scout_contact_time=NOW() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_last_scout_observed_version(
    machine_id: &MachineId,
    scout_version: &str,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_scout_observed_version=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(scout_version)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn find_host_by_dpu_machine_id(
    txn: &mut PgConnection,
    dpu_machine_id: &DpuMachineId,
) -> Result<Option<HostMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{} WHERE m.id = ANY(ARRAY(
                    SELECT mi.machine_id FROM machine_interfaces mi
                    WHERE mi.attached_dpu_machine_id = $1
                    AND mi.interface_type != 'Bmc'
                    AND mi.attached_dpu_machine_id != mi.machine_id))"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(dpu_machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

/// `lookup_host_state_by_dpu_machine_id` returns the associated host's state
/// by reading only `id` and `controller_state` through a non-BMC interface
/// owned by a machine other than the requested DPU.
///
/// The owner must have a stable or predicted [`HostMachineId`]. Invalid IDs
/// or state data return decoding errors; no matching association returns `None`.
/// Selection is unspecified when multiple hosts match. Other snapshot fields
/// are neither read nor decoded; use [`find_host_by_dpu_machine_id`] for those.
pub async fn lookup_host_state_by_dpu_machine_id(
    conn: impl DbReader<'_>,
    dpu_machine_id: &DpuMachineId,
) -> DatabaseResult<Option<ManagedHostState>> {
    let query = r#"SELECT m.id, m.controller_state
        FROM machines m
        INNER JOIN machine_interfaces mi ON m.id = mi.machine_id
        WHERE mi.attached_dpu_machine_id = $1
          AND mi.interface_type != 'Bmc'
          AND mi.attached_dpu_machine_id != mi.machine_id
        LIMIT 1"#;

    let host: Option<(HostMachineId, sqlx::types::Json<ManagedHostState>)> = sqlx::query_as(query)
        .bind(dpu_machine_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(host.map(|(_, state)| state.0))
}

/// DPU-to-host attachments found for one requested set of DPUs.
///
/// Loading permits partial results and DPUs from different hosts. Call
/// [`Self::common_host_machine_id`] to require one host for the complete set.
#[derive(Debug)]
pub struct DpuHostAssociations {
    host_ids_by_dpu_id: HashMap<DpuMachineId, HostMachineId>,
    missing_dpu_machine_id: Option<DpuMachineId>,
}

/// A requested DPU set cannot be interpreted as one existing managed host.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DpuHostAssociationError {
    /// Some requested DPUs have host attachments, but this one does not.
    #[error("DPU {dpu_machine_id} is not attached to a host while other requested DPUs are")]
    Incomplete {
        /// First requested DPU without a host attachment.
        dpu_machine_id: DpuMachineId,
    },
    /// All requested DPUs have attachments, but to more than one host.
    #[error("requested DPUs are attached to different hosts")]
    MultipleHosts,
}

impl DpuHostAssociations {
    /// Loads non-BMC host attachments for the requested DPUs.
    ///
    /// A DPU's own interfaces are excluded. Missing DPUs are allowed, and
    /// duplicate requested IDs do not change the result.
    pub async fn load(
        conn: impl DbReader<'_>,
        dpu_machine_ids: &[DpuMachineId],
    ) -> DatabaseResult<Self> {
        let query = r#"SELECT mi.attached_dpu_machine_id, mi.machine_id
            FROM machine_interfaces mi
            WHERE mi.attached_dpu_machine_id != mi.machine_id
            AND mi.interface_type != 'Bmc'
            AND mi.attached_dpu_machine_id = ANY($1)"#;

        let pairs: Vec<(DpuMachineId, HostMachineId)> = sqlx::query_as(query)
            .bind(
                dpu_machine_ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>(),
            )
            .fetch_all(conn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        let host_ids_by_dpu_id: HashMap<_, _> = pairs.into_iter().collect();
        let missing_dpu_machine_id = dpu_machine_ids
            .iter()
            .find(|id| !host_ids_by_dpu_id.contains_key(id))
            .copied();
        Ok(Self {
            host_ids_by_dpu_id,
            missing_dpu_machine_id,
        })
    }

    /// Returns the host attached to a requested DPU, if one was found.
    pub fn host_machine_id(&self, dpu_machine_id: &DpuMachineId) -> Option<HostMachineId> {
        self.host_ids_by_dpu_id.get(dpu_machine_id).copied()
    }

    /// Returns one host ID per attached DPU; IDs may repeat and have no defined order.
    pub fn host_machine_ids(&self) -> impl Iterator<Item = HostMachineId> + '_ {
        self.host_ids_by_dpu_id.values().copied()
    }

    /// Returns `None` when no requested DPU has a host attachment, including an empty request.
    /// Returns a host only when every requested DPU is attached to that same host.
    ///
    /// Partial attachment takes precedence over conflicting hosts when both occur.
    pub fn common_host_machine_id(&self) -> Result<Option<HostMachineId>, DpuHostAssociationError> {
        match self.missing_dpu_machine_id {
            Some(dpu_machine_id) if !self.host_ids_by_dpu_id.is_empty() => {
                Err(DpuHostAssociationError::Incomplete { dpu_machine_id })
            }
            _ => self
                .host_machine_ids()
                .try_fold(None, |common_host_id, host_id| match common_host_id {
                    Some(common_host_id) if common_host_id != host_id => {
                        Err(DpuHostAssociationError::MultipleHosts)
                    }
                    _ => Ok(Some(host_id)),
                }),
        }
    }
}

/// `find_states_by_ids` reads each machine's own controller state without loading
/// the full machine snapshot. Results contain one row per existing requested
/// machine, sorted by ID.
/// Missing IDs are omitted; a query or state-decoding error fails the whole read.
pub async fn find_states_by_ids(
    conn: impl DbReader<'_>,
    machine_ids: &[MachineId],
) -> DatabaseResult<Vec<(MachineId, ManagedHostState)>> {
    let query = "SELECT id, controller_state FROM machines WHERE id = ANY($1) ORDER BY id";
    let rows: Vec<(MachineId, sqlx::types::Json<ManagedHostState>)> = sqlx::query_as(query)
        .bind(machine_ids)
        .fetch_all(conn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(rows.into_iter().map(|(id, state)| (id, state.0)).collect())
}

/// Return the [`ManagedHostState`] for a machine given its id without returning the whole snapshot.
pub async fn lookup_managed_host_state(
    conn: impl DbReader<'_>,
    machine_id: MachineId,
) -> DatabaseResult<Option<ManagedHostState>> {
    let query = "SELECT controller_state FROM machines WHERE id = $1";

    let Some(json): Option<sqlx::types::Json<ManagedHostState>> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?
    else {
        return Ok(None);
    };

    Ok(Some(json.0))
}

/// `find_host_nic_firmware_operations` reads only frozen operations from current
/// stable or predicted host controller states, paired with each owning host ID.
/// DPU copies and historical states are excluded. Callers validate operations
/// and decide which phases retain artifacts; a malformed matching operation
/// fails the entire read without returning a partial retention set.
pub async fn find_host_nic_firmware_operations(
    conn: impl DbReader<'_>,
) -> DatabaseResult<Vec<(HostMachineId, NicFirmwareOperation)>> {
    let query = r#"
        SELECT id, controller_state->'operation' FROM machines
        WHERE (id LIKE $1 OR id LIKE $2)
          AND controller_state->>'state' = 'nicfirmwareupdating'
    "#;
    let operations: Vec<(HostMachineId, sqlx::types::Json<NicFirmwareOperation>)> =
        sqlx::query_as(query)
            .bind(format!("{}%", MachineType::Host.id_prefix()))
            .bind(format!("{}%", MachineType::PredictedHost.id_prefix()))
            .fetch_all(conn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    Ok(operations
        .into_iter()
        .map(|(host_machine_id, operation)| (host_machine_id, operation.0))
        .collect())
}

/// Returns the `use_admin_network` flag from the host that owns the
/// given DPA interface.
pub async fn get_host_use_admin_network_for_dpa_interface(
    conn: impl DbReader<'_>,
    dpa_interface_id: &DpaInterfaceId,
) -> Result<bool, DatabaseError> {
    let query = r#"
        SELECT COALESCE((h.network_config->>'use_admin_network')::bool, true)
        FROM dpa_interfaces da
        JOIN machines h ON h.id = da.machine_id
        WHERE da.id = $1
        LIMIT 1"#;
    let flag: Option<bool> = sqlx::query_scalar(query)
        .bind(dpa_interface_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(flag.unwrap_or(true))
}

lazy_static! {
    static ref FIND_DPUS_BY_HOST_MACHINE_ID_QUERY: String = format!(
        r#"{}
                WHERE m.id = ANY(ARRAY(
                    SELECT mi.attached_dpu_machine_id FROM machine_interfaces mi
                    WHERE mi.machine_id = $1
                    AND mi.interface_type != 'Bmc'
                    AND mi.attached_dpu_machine_id IS NOT NULL))"#,
        JSON_MACHINE_SNAPSHOT_QUERY.deref()
    );
}

/// Load the DPUs attached to a host through any non-BMC host interface.
///
/// Each attached DPU is returned once, even when several host interfaces
/// attach to it.
pub async fn find_dpus_by_host_machine_id(
    txn: &mut PgConnection,
    host_machine_id: &HostMachineId,
) -> Result<Vec<DpuMachine>, DatabaseError> {
    let query = FIND_DPUS_BY_HOST_MACHINE_ID_QUERY.as_str();
    let machines = sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(host_machine_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(machines)
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    metadata: Metadata,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let query = "UPDATE machines SET
            version=$1,
            name=$2, description=$3, labels=$4::jsonb
            WHERE id=$5 AND version=$6
            RETURNING id";
    let query_result: Result<(MachineId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(machine_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok((_machine_id,)) => Ok(()),
        Err(e) => Err(match e {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("machine", expected_version.to_string())
            }
            e => DatabaseError::query(query, e),
        }),
    }
}

/// `MachineObservationNotCurrent` means the machine is missing or its stored
/// observation no longer satisfies the timestamp condition. The write does not
/// distinguish those cases. Converting this reason to [`DatabaseError`] wraps
/// `sqlx::Error::RowNotFound` for callers that require the observation to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineObservationNotCurrent;

impl From<MachineObservationNotCurrent> for DatabaseError {
    #[track_caller]
    fn from(_: MachineObservationNotCurrent) -> Self {
        Self::new(
            "update machine status observation",
            sqlx::Error::RowNotFound,
        )
    }
}

/// Stores a network observation when its timestamp is at least as recent as the
/// stored one, or no timestamp is stored. Compares both JSON timestamps at
/// PostgreSQL's microsecond precision. The server-owned accepted LLDP
/// fingerprint is preserved from the locked row. Returns `NotApplied` for a
/// missing machine or rejected timestamp.
pub async fn update_network_status_observation(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    observation: &MachineNetworkStatusObservation,
) -> Result<ConditionalWrite<(), MachineObservationNotCurrent>, DatabaseError> {
    // Parse both timestamps from JSON so they round alike. SQLx truncates
    // separately bound timestamps to microseconds.
    let query = "UPDATE machines SET network_status_observation =
                    ($1::jsonb - 'accepted_lldp_snapshot_fingerprint')
                    || CASE
                        WHEN network_status_observation ? 'accepted_lldp_snapshot_fingerprint'
                        THEN jsonb_build_object(
                            'accepted_lldp_snapshot_fingerprint',
                            network_status_observation->'accepted_lldp_snapshot_fingerprint'
                        )
                        ELSE '{}'::jsonb
                    END
                WHERE id = $2 AND
                (
                    (network_status_observation->>'observed_at' IS NULL)
                    OR ((network_status_observation->>'observed_at')::timestamp
                        <= ($1::jsonb->>'observed_at')::timestamp)
                ) RETURNING id";
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if updated.is_some() {
        return Ok(ConditionalWrite::Applied(()));
    }

    debug_failed_machine_status_update(txn, machine_id, "network_status_observation", observation)
        .await;
    Ok(ConditionalWrite::NotApplied(MachineObservationNotCurrent))
}

/// Stores the accepted fingerprint. Call in the same transaction as the
/// corresponding neighbor replacement.
pub async fn update_accepted_lldp_snapshot_fingerprint(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    fingerprint: &[u8],
) -> Result<(), DatabaseError> {
    let query = r#"
        UPDATE machines
        SET network_status_observation = jsonb_set(
            network_status_observation,
            '{accepted_lldp_snapshot_fingerprint}',
            $1::jsonb,
            true
        )
        WHERE id = $2
          AND network_status_observation IS NOT NULL
    "#;
    let result = sqlx::query(query)
        .bind(sqlx::types::Json(fingerprint))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "machine network status observation",
            id: machine_id.to_string(),
        });
    }
    Ok(())
}

/// Returns the accepted fingerprint while locking the machine row for the
/// caller's transaction.
pub async fn lock_accepted_lldp_snapshot_fingerprint(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
) -> Result<Option<Vec<u8>>, DatabaseError> {
    let query = r#"
        SELECT network_status_observation->'accepted_lldp_snapshot_fingerprint'
        FROM machines
        WHERE id = $1
        FOR UPDATE
    "#;
    let fingerprint: Option<Option<sqlx::types::Json<Vec<u8>>>> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    fingerprint
        .map(|value| value.map(|json| json.0))
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })
}

/// `ExtensionServiceObservationNotCurrent` means the conditional observation
/// update did not match, but the machine was present when rechecked. The
/// rejected observation does not replace the stored service entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionServiceObservationNotCurrent;

/// Updates the current extension-service observation for one service type.
///
/// Writers own their type key, so the DPU agent's KubernetesPod observation
/// and NICo's DPF Helm observation cannot replace one another. The operation
/// is a single JSONB update: PostgreSQL serializes concurrent row updates and
/// `jsonb_set` retains every other service-type entry.
///
/// Compares the stored and incoming JSON timestamps at PostgreSQL's microsecond
/// precision, accepting equal timestamps and newer observations.
///
/// Returns `Applied(())` when the observation is stored. A conditional miss
/// returns `NotApplied` if the machine exists when rechecked, or
/// [`DatabaseError::NotFoundError`] if it is absent. The identity recheck is
/// a separate read, not part of the timestamp comparison's snapshot.
pub async fn update_extension_service_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    service_type: ExtensionServiceType,
    observation: &InstanceExtensionServiceStatusObservation,
) -> Result<ConditionalWrite<(), ExtensionServiceObservationNotCurrent>, DatabaseError> {
    let query = r#"
        UPDATE machines
        SET extension_service_status_observations = jsonb_set(
            COALESCE(extension_service_status_observations, '{}'::jsonb),
            ARRAY[$2]::text[],
            $3::jsonb,
            true
        )
        WHERE id = $1
          AND (
              extension_service_status_observations -> $2 IS NULL
              OR (extension_service_status_observations -> $2 ->> 'observed_at')::timestamptz
                    <= ($3::jsonb->>'observed_at')::timestamptz
          )
        RETURNING id
    "#;
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(machine_id)
        .bind(service_type.to_string())
        .bind(sqlx::types::Json(observation))
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if updated.is_some() {
        return Ok(ConditionalWrite::Applied(()));
    }

    // The update above matches on machine identity and observation freshness
    // together, so re-check identity alone to attribute the miss to one or the
    // other.
    let identity_query = "SELECT id FROM machines WHERE id = $1";
    let machine_exists: Option<(MachineId,)> = sqlx::query_as(identity_query)
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(identity_query, e))?;
    if machine_exists.is_some() {
        return Ok(ConditionalWrite::NotApplied(
            ExtensionServiceObservationNotCurrent,
        ));
    }

    // Captures why the update failed in unit tests even though all prerequisite
    // data appears present. Compiles to a no-op in production environments.
    debug_failed_machine_status_update(
        txn,
        machine_id,
        "extension_service_status_observations",
        observation,
    )
    .await;
    Err(DatabaseError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })
}

/// Stores an InfiniBand observation when none is stored or its timestamp is at
/// least as recent, comparing both JSON timestamps at PostgreSQL's microsecond
/// precision. A missing machine or an unmet condition returns `NotApplied`.
pub async fn update_infiniband_status_observation(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    observation: &MachineInfinibandStatusObservation,
) -> Result<ConditionalWrite<(), MachineObservationNotCurrent>, DatabaseError> {
    let query = "UPDATE machines SET infiniband_status_observation = $1::json WHERE id = $2 AND
             (infiniband_status_observation IS NULL
                OR (infiniband_status_observation ? 'observed_at'
                    AND (infiniband_status_observation->>'observed_at')::timestamptz
                        <= ($1::json->>'observed_at')::timestamptz)
            ) RETURNING id";
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match updated {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(MachineObservationNotCurrent),
    })
}

/// Stores an NVLink observation when no timestamp is stored or its timestamp is
/// at least as recent, comparing both JSON timestamps at PostgreSQL's
/// microsecond precision. A missing machine or an unmet condition returns
/// `NotApplied`.
pub async fn update_nvlink_status_observation(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    observation: &MachineNvLinkStatusObservation,
) -> Result<ConditionalWrite<(), MachineObservationNotCurrent>, DatabaseError> {
    let query = "UPDATE machines SET nvlink_status_observation = $1::json WHERE id = $2 AND
                (nvlink_status_observation->>'observed_at' IS NULL
                    OR (nvlink_status_observation->>'observed_at')::timestamptz
                        <= ($1::json->>'observed_at')::timestamptz) RETURNING id";
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match updated {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(MachineObservationNotCurrent),
    })
}

/// Clears `machines.nvlink_status_observation` for the given machine IDs.
///
/// Used when NMX-C is unreachable so instance state does not retain stale
/// partition observations. Rows already holding NULL are not written, so the
/// `updated` trigger does not fire for them.
pub async fn clear_nvlink_status_observations(
    txn: &mut PgConnection,
    machine_ids: &[HostMachineId],
) -> Result<(), DatabaseError> {
    if machine_ids.is_empty() {
        return Ok(());
    }

    let query = "UPDATE machines SET nvlink_status_observation = NULL
                 WHERE id = ANY($1) AND nvlink_status_observation IS NOT NULL";
    let machine_id_strings: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();
    sqlx::query(query)
        .bind(machine_id_strings)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Stores a Spectrum-X observation when no timestamp is stored or its timestamp
/// is at least as recent, comparing both JSON timestamps at PostgreSQL's
/// microsecond precision. A missing machine or an unmet condition returns
/// `NotApplied`.
pub async fn update_spx_status_observation(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    observation: &MachineSpxStatusObservation,
) -> Result<ConditionalWrite<(), MachineObservationNotCurrent>, DatabaseError> {
    tracing::debug!(
        observation = ?observation,
        "updating SPX status observation",
    );
    let query = "UPDATE machines SET spx_status_observation = $1::json WHERE id = $2 AND
                (spx_status_observation->>'observed_at' IS NULL
                    OR (spx_status_observation->>'observed_at')::timestamptz
                        <= ($1::json->>'observed_at')::timestamptz) RETURNING id";
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(sqlx::types::Json(&observation))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match updated {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(MachineObservationNotCurrent),
    })
}

/// Lock the host row and read its current maintenance requests. Keep the
/// transaction open until the dependent update is committed.
pub async fn lock_spx_multiplane(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
) -> DatabaseResult<Option<MachineSpxMultiplane>> {
    let query = "SELECT spx_multiplane FROM machines WHERE id=$1 FOR UPDATE";
    let configuration: Option<Option<sqlx::types::Json<MachineSpxMultiplane>>> =
        sqlx::query_scalar(query)
            .bind(machine_id)
            .fetch_optional(txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    configuration
        .map(|value| value.map(|json| json.0))
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })
}

/// Replace maintenance state while holding the lock acquired by
/// `lock_spx_multiplane` in the same transaction.
pub async fn update_spx_multiplane(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    configuration: &MachineSpxMultiplane,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET spx_multiplane=$1 WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(configuration))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// Lock the reporting DPU row before merging a partial maintenance report.
/// Callers lock the host configuration first to serialize reports against desired revisions.
pub async fn lock_spx_multiplane_status(
    txn: &mut PgConnection,
    dpu_machine_id: &DpuMachineId,
) -> DatabaseResult<Option<MachineSpxMultiplaneStatus>> {
    let query = "SELECT spx_multiplane_status FROM machines WHERE id=$1 FOR UPDATE";
    let status: Option<Option<sqlx::types::Json<MachineSpxMultiplaneStatus>>> =
        sqlx::query_scalar(query)
            .bind(dpu_machine_id)
            .fetch_optional(txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
    status
        .map(|value| value.map(|json| json.0))
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: dpu_machine_id.to_string(),
        })
}

/// Store only the reporting DPU's status while holding its row lock.
pub async fn update_spx_multiplane_status(
    txn: &mut PgConnection,
    dpu_machine_id: &DpuMachineId,
    status: &MachineSpxMultiplaneStatus,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET spx_multiplane_status=$1 WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(status))
        .bind(dpu_machine_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(())
}

/// `update_mlx_device_observation` saves an unfiltered observation if none
/// exists or the new timestamp is at least as recent. Comparisons use the
/// database's timestamp precision. Returns `NotApplied` if the machine is
/// missing or a newer observation is stored. Callers must validate report
/// ownership and completeness. Accepted reports get `received_at` from the
/// database clock; any value supplied by the caller is replaced.
pub async fn update_mlx_device_observation(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    observation: &MlxDeviceObservation,
) -> Result<ConditionalWrite<(), MachineObservationNotCurrent>, DatabaseError> {
    let query = "UPDATE machines SET mlx_device_observation =
                    jsonb_set($1::jsonb, '{received_at}', to_jsonb(clock_timestamp()))
                WHERE id = $2 AND
                (mlx_device_observation IS NULL
                    OR (mlx_device_observation->>'observed_at')::timestamptz
                        <= ($1::json->>'observed_at')::timestamptz) RETURNING id";
    let updated: Option<(MachineId,)> = sqlx::query_as(query)
        .bind(sqlx::types::Json(observation))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(match updated {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(MachineObservationNotCurrent),
    })
}

#[cfg(test)]
async fn debug_failed_machine_status_update(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    column_name: &str,
    column_data: &impl serde::Serialize,
) {
    let serialized_data =
        serde_json::to_string_pretty(column_data).unwrap_or_else(|_| "Invalid".to_string());
    tracing::error!(
        machine_id=%machine_id,
        column_name,
        serialized_data = %serialized_data,
        "Failed to update column. New column data",
    );
    // Dump the raw Machine state for debugging purposes
    let query = "SELECT * from machines WHERE id = $1";
    match sqlx::query(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
    {
        Ok(Some(row)) => {
            tracing::error!(
                row = ?row,
                "Machine Data",
            );
        }
        Ok(None) => {
            tracing::error!("No Machine Data");
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to load machine data",
            );
        }
    }
}

#[cfg(not(test))]
#[allow(clippy::unused_async)]
async fn debug_failed_machine_status_update(
    _txn: &mut PgConnection,
    _machine_id: &MachineId,
    _column_name: &str,
    _column_data: &impl serde::Serialize,
) {
}

pub async fn update_dpu_agent_health_report(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    let mut health_report = health_report.clone();
    health_report.source = HealthReport::DPU_AGENT_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_machine_validation_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::MACHINE_VALIDATION_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::MACHINE_VALIDATION_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_site_explorer_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::SITE_EXPLORER_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::SITE_EXPLORER_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn update_sku_validation_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    if health_report.alerts.is_empty() {
        return crate::health_report::remove_health_report(
            txn,
            "machines",
            machine_id,
            HealthReportApplyMode::Merge,
            HealthReport::SKU_VALIDATION_SOURCE,
        )
        .await;
    }

    let mut health_report = health_report.clone();
    health_report.source = HealthReport::SKU_VALIDATION_SOURCE.to_string();
    crate::health_report::insert_health_report(
        txn,
        "machines",
        machine_id,
        HealthReportApplyMode::Merge,
        &health_report,
    )
    .await
}

pub async fn insert_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
    no_overwrite: bool,
) -> Result<(), DatabaseError> {
    if no_overwrite {
        // TODO(chet): This appears to be a machine-specific thing -- skip insert
        // if a merge with the same source already exists -- but I'm not sure what
        // it's used for, since others seem to do a remove + insert. Do we need
        // to support this still? Might be nice to explain it somewhere.
        let path = match mode {
            HealthReportApplyMode::Merge => {
                vec!["merges".to_string(), health_report.source.clone()]
            }
            HealthReportApplyMode::Replace => vec!["replace".to_string()],
        };

        let mut query = sqlx::QueryBuilder::new(
            "UPDATE machines SET health_reports = jsonb_set(
                coalesce(health_reports, '{\"merges\": {}}'::jsonb),
                ",
        );
        query.push_bind(path);
        query.push(", ");
        query.push_bind(sqlx::types::Json(health_report));
        query.push(") WHERE id = ");
        query.push_bind(machine_id);
        query.push(" AND coalesce(health_reports, '{\"merges\": {}}'::jsonb)->'merges' ? ");
        query.push_bind(&health_report.source);
        query.push(" = FALSE RETURNING id");

        let _id: (MachineId,) = query
            .build_query_as()
            .fetch_one(txn)
            .await
            .map_err(|e| DatabaseError::new("insert health report override", e))?;

        Ok(())
    } else {
        crate::health_report::insert_health_report(txn, "machines", machine_id, mode, health_report)
            .await
    }
}

pub async fn remove_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError> {
    crate::health_report::remove_health_report(txn, "machines", machine_id, mode, source).await
}

pub async fn update_agent_reported_inventory(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    inventory: &MachineInventory,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET agent_reported_inventory = $1::json WHERE id = $2 RETURNING id";
    tracing::debug!(machine_id = %machine_id, "Updating machine inventory");
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(&inventory))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn get_all_network_status_observation(
    txn: impl DbReader<'_>,
    limit: i64, // return at most this many rows
) -> Result<Vec<MachineNetworkStatusObservation>, DatabaseError> {
    let query = "SELECT network_status_observation FROM machines
            WHERE network_status_observation IS NOT NULL
            ORDER BY network_status_observation->'machine_id'
            LIMIT $1::integer";
    let rows = sqlx::query(query)
        .bind(limit)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    let mut all = Vec::with_capacity(rows.len());
    for row in rows {
        let s: sqlx::types::Json<MachineNetworkStatusObservation> = row
            .try_get("network_status_observation")
            .map_err(|e| DatabaseError::query(query, e))?;
        all.push(s.0);
    }
    Ok(all)
}

/// Force cleans all tables related to a Machine - except MachineInterfaces
///
/// DO NOT USE OUTSIDE OF ADMIN CLI
pub async fn force_cleanup(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    // Note: It might be nicer to actually write the full query here so we can
    // report more results.
    // But this way we at least unit-test the stored procedure and make sure
    // it stays up to date.
    let query = r#"call cleanup_machine_by_id($1)"#;
    let _query_result = sqlx::query(query)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// `MachineNetworkConfigNotCurrent` means the target is missing or its network
/// version no longer matches. The guarded backfill also rejects force deletion;
/// the write deliberately does not distinguish these cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineNetworkConfigNotCurrent;

/// Updates the `network_config` on the target machine row (host or
/// DPU), and bumps the `network_config_version` on *every* machine
/// row in the same "machine group" (host + every DPU attached to it)
/// to the same new version, so all rows in the group stay version-equal.
///
/// The caller can pass any machine ID in the group; group membership is
/// computed by the `machine_group_member_ids` Postgres function, which
/// walks `machine_interfaces`. For zero-DPU hosts, the group is just the
/// target itself.
///
/// Returns `NotApplied(MachineNetworkConfigNotCurrent)` for a missing target or
/// changed version. Other group rows are locked in Machine ID order with
/// NO KEY UPDATE before receiving the new version. Host targets wait; DPU
/// targets use NOWAIT because they already hold a DPU before reaching the host.
/// SQLSTATE 55P03 is a database error, not a version rejection. Roll back the
/// whole transaction on error; commit the target and group writes together.
pub async fn try_update_network_config(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    new_state: &ManagedHostNetworkConfig,
) -> Result<ConditionalWrite<(), MachineNetworkConfigNotCurrent>, DatabaseError> {
    try_update_network_config_inner(txn, machine_id, expected_version, new_state, false, None).await
}

async fn try_update_network_config_unless_force_deleting(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    new_state: &ManagedHostNetworkConfig,
) -> Result<ConditionalWrite<(), MachineNetworkConfigNotCurrent>, DatabaseError> {
    try_update_network_config_inner(txn, machine_id, expected_version, new_state, true, None).await
}

/// `try_update_network_config_for_created` writes network configuration only
/// to the same machine row and version. ForceDeletion, replacement rows,
/// missing rows, and changed versions return `NotApplied`.
/// The caller attaches allocations separately in the same transaction and
/// rolls back both changes if this write does not apply.
/// Group locking and errors follow [`try_update_network_config`], including
/// DPU NOWAIT contention. Roll back the whole transaction on error.
pub async fn try_update_network_config_for_created(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    created: DateTime<Utc>,
    expected_version: ConfigVersion,
    new_state: &ManagedHostNetworkConfig,
) -> Result<ConditionalWrite<(), MachineNetworkConfigNotCurrent>, DatabaseError> {
    try_update_network_config_inner(
        txn,
        machine_id,
        expected_version,
        new_state,
        true,
        Some(created),
    )
    .await
}

async fn is_force_deleting_or_deleted(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT controller_state = $2::jsonb FROM machines WHERE id = $1";
    let is_force_deleting: Option<bool> = sqlx::query_scalar(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(is_force_deleting.unwrap_or(true))
}

async fn try_update_network_config_inner(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    expected_version: ConfigVersion,
    new_state: &ManagedHostNetworkConfig,
    reject_force_deletion: bool,
    expected_created: Option<DateTime<Utc>>,
) -> Result<ConditionalWrite<(), MachineNetworkConfigNotCurrent>, DatabaseError> {
    let next_version = expected_version.increment();

    // First, do our usual "optimistic" lock update on the target row, which
    // writes content and bumps to the next_version (which is conditional on
    // the row currently being at `expected_version`).
    let target_query = "UPDATE machines SET network_config_version=$1, network_config=$2::json
            WHERE id=$3 AND network_config_version=$4
              AND (NOT $5 OR controller_state != $6::jsonb)
              AND ($7::timestamptz IS NULL OR created = $7)
            RETURNING id";
    let target_result: Result<MachineId, _> = sqlx::query_as(target_query)
        .bind(next_version)
        .bind(sqlx::types::Json(new_state))
        .bind(machine_id)
        .bind(expected_version)
        .bind(reject_force_deletion)
        .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
        .bind(expected_created)
        .fetch_one(&mut *txn)
        .await;

    match target_result {
        Ok(_) => {
            // Lock exactly the rows this statement updates, in a shared order.
            // A DPU-target writer must not wait for a host that already owns
            // the group write and is waiting for that target DPU.
            const LOCK_GROUP: &str = r#"
                WITH locked_group AS MATERIALIZED (
                    SELECT m.id
                    FROM machines AS m
                    WHERE m.id != $2
                      AND m.id IN (SELECT id FROM machine_group_member_ids($2))
                    ORDER BY m.id COLLATE "C"
                    FOR NO KEY UPDATE OF m
            "#;
            const UPDATE_GROUP: &str = r#"
                )
                UPDATE machines AS m
                SET network_config_version = $1
                FROM locked_group AS g
                WHERE m.id = g.id
            "#;
            let group_query = if machine_id.machine_type().is_dpu() {
                const_format::concatcp!(LOCK_GROUP, " NOWAIT", UPDATE_GROUP)
            } else {
                const_format::concatcp!(LOCK_GROUP, UPDATE_GROUP)
            };

            sqlx::query(group_query)
                .bind(next_version)
                .bind(machine_id)
                .execute(&mut *txn)
                .await
                .map_err(|e| DatabaseError::query(group_query, e))?;
            Ok(ConditionalWrite::Applied(()))
        }
        Err(sqlx::Error::RowNotFound) => {
            Ok(ConditionalWrite::NotApplied(MachineNetworkConfigNotCurrent))
        }
        Err(e) => Err(DatabaseError::query(target_query, e)),
    }
}

/// Sets or clears the `use_admin_network_changed` flag on a single machine
/// row without bumping `network_config_version` or fanning out to the group.
pub async fn set_use_admin_network_changed(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    value: bool,
) -> Result<(), DatabaseError> {
    let query = r#"
        UPDATE machines
        SET network_config = jsonb_set(COALESCE(network_config, '{}'::jsonb), '{use_admin_network_changed}', $1::jsonb)
        WHERE id = $2
    "#;
    let result = sqlx::query(query)
        .bind(sqlx::types::Json(value))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if result.rows_affected() == 0 {
        return Err(DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        });
    }
    Ok(())
}

/// `AdminNetworkChangeNotPending` means no pending flag matches the acknowledged
/// version. The machine may be missing, its network config version may differ,
/// or `use_admin_network_changed` may already be false or unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminNetworkChangeNotPending;

/// Clears the pending `use_admin_network_changed` flag only if the machine is
/// still at the expected network config version, without advancing that version.
/// Returns `NotApplied(AdminNetworkChangeNotPending)` when no pending flag matches;
/// database failures remain errors.
pub async fn clear_use_admin_network_changed_if_version_matches(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    expected_version: &ConfigVersion,
) -> Result<ConditionalWrite<(), AdminNetworkChangeNotPending>, DatabaseError> {
    let query = r#"
        UPDATE machines
        SET network_config = jsonb_set(COALESCE(network_config, '{}'::jsonb), '{use_admin_network_changed}', 'false'::jsonb)
        WHERE id = $1
          AND network_config_version = $2
          AND network_config -> 'use_admin_network_changed' = 'true'::jsonb
    "#;
    let result = sqlx::query(query)
        .bind(machine_id)
        .bind(expected_version)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(if result.rows_affected() > 0 {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(AdminNetworkChangeNotPending)
    })
}

/// Locks the interface rows of `predicted_host_id`, ascending by id and BMC
/// rows included, then its machines row: the order the host's other writers
/// use. Call it before any other row lock in the transaction, so the FK
/// cascade of [`try_sync_stable_id_with_current_machine_id_for_host`] finds
/// the interface rows already held; the rows it cascades into through the
/// other tables that reference `machines(id)` are ones other flows write only
/// after locking the machines row. Locks nothing when the host was renamed or
/// deleted since the caller read its id.
pub async fn lock_rows_for_stable_id_sync(
    txn: &mut PgConnection,
    predicted_host_id: &PredictedHostMachineId,
) -> DatabaseResult<()> {
    let query = "SELECT id FROM machine_interfaces WHERE machine_id = $1 ORDER BY id FOR UPDATE";
    sqlx::query(query)
        .bind(predicted_host_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    let query = "SELECT id FROM machines WHERE id = $1 FOR UPDATE";
    sqlx::query(query)
        .bind(predicted_host_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Replaces predicted host id with stable host id.
/// Once forge receives DiscoveryData from Host, forge can create StableMachineId.
/// This StableMachineId must replace existing PredictedHostId in db.
/// State machine does not act on receiving discoverydata, but discoverycompleted message,
/// so updating host id must not interfere state machine handling.
/// Callers hold the locks from [`lock_rows_for_stable_id_sync`], so the FK
/// cascade of the rename below finds the host's interface rows already held
/// and reaches the other referencing tables only after the machines row.
pub async fn try_sync_stable_id_with_current_machine_id_for_host(
    txn: &mut PgConnection,
    current_machine_id: Option<HostMachineId>,
    stable_machine_id: &StableHostMachineId,
) -> Result<StableHostMachineId, DatabaseError> {
    let Some(current_machine_id) = current_machine_id else {
        return Err(DatabaseError::NotFoundError {
            kind: "machine_id",
            id: stable_machine_id.to_string(),
        });
    };

    // This is repeated call. Machine is already updated with stable ID.
    if let Ok(stable_host_id) = StableHostMachineId::try_from(current_machine_id) {
        return match find_one(txn, &stable_host_id, MachineSearchConfig::default()).await? {
            Some(machine) => Ok(machine.id),
            None => Err(DatabaseError::NotFoundError {
                kind: "machine",
                id: current_machine_id.to_string(),
            }),
        };
    }

    // Update the machine state and health history to account for the rename
    crate::state_history::update_object_ids(
        txn,
        crate::state_history::StateHistoryTableId::Machine,
        &current_machine_id,
        &stable_machine_id,
    )
    .await?;
    crate::health_history::update_object_ids(
        txn,
        crate::health_history::HealthHistoryTableId::Machine,
        &current_machine_id,
        &stable_machine_id,
    )
    .await?;

    // dpa_interfaces has a UNIQUE(machine_id, mac_address) constraint
    // (unique_mid_mac). If the stable id already owns a row with the same MAC
    // (for example, a prior partially completed rename), the cascade below would
    // collide. Drop those stale stable-id rows first so the predicted host's
    // freshly discovered rows win when they are renamed onto the stable id.
    let query = "DELETE FROM dpa_interfaces \
                 WHERE machine_id=$1 \
                   AND mac_address IN (SELECT mac_address FROM dpa_interfaces WHERE machine_id=$2)";
    sqlx::query(query)
        .bind(stable_machine_id)
        .bind(current_machine_id)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // Interface and NIC firmware selection rows reference machines(id) with
    // ON UPDATE CASCADE, so their machine_id follows this rename automatically.
    let query = "UPDATE machines SET id=$1 WHERE id=$2 RETURNING id";
    let machine_id = sqlx::query_as(query)
        .bind(stable_machine_id)
        .bind(current_machine_id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // If the Machines name in Metadata matched the predicted machine id,
    // then update it to the new ID.
    // If someone changed the name manually then don't bother
    let query = "UPDATE machines SET name=$1 WHERE id=$2 AND name=$3";
    sqlx::query(query)
        .bind(stable_machine_id)
        .bind(stable_machine_id)
        .bind(current_machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    tracing::info!(
        "Synced stable id {stable_machine_id} with current machine id {current_machine_id}"
    );

    Ok(machine_id)
}

pub async fn update_failure_details<ID: MachineIdSubtypeTrait>(
    machine: &Machine<ID>,
    txn: &mut PgConnection,
    failure: FailureDetails,
) -> Result<(), DatabaseError> {
    update_failure_details_by_machine_id(machine.id.as_machine_id(), txn, failure).await
}

pub async fn clear_failure_details(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let failure_details = FailureDetails {
        cause: model::machine::FailureCause::NoError,
        failed_at: chrono::Utc::now(),
        source: model::machine::FailureSource::NoError,
    };

    let query = "UPDATE machines SET failure_details = $1::json WHERE id = $2 RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(failure_details))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Create new machine in DB.
///
/// If metadata is not specified then default metadata is used
/// with name set to stable_machine_id.
///
/// If metadata.name is empty then it is initialized in
/// stable_machine_id.to_string().
///
/// `common_pools` supplies Integrated DPU ASN and optional IPv6 allocation.
/// Callers preparing ASNs separately use [`create_with_requested_loopback`] with
/// [`MachineCreationPools::Ipv6Only`] to preserve IPv6 allocation during creation.
pub async fn create(
    txn: &mut PgConnection,
    common_pools: Option<&CommonPools>,
    stable_machine_id: &MachineId,
    state: ManagedHostState,
    expected_machine_data: Option<&ExpectedMachineData>,
    state_model_version: i16,
) -> DatabaseResult<AnyMachine> {
    create_with_requested_loopback(
        txn,
        common_pools.map(MachineCreationPools::All),
        stable_machine_id,
        state,
        expected_machine_data,
        state_model_version,
        None,
    )
    .await
}

/// `MachineCreationPools` selects the Integrated allocations made with a new row.
/// Passing `None` to creation skips all allocation. IPv4 loopbacks attach separately.
pub enum MachineCreationPools<'a> {
    /// Preserve normal creation: best-effort ASN and optional IPv6 loopback.
    All(&'a CommonPools),
    /// Allocate IPv6 normally while a prepared ASN attaches after interface writes.
    Ipv6Only(&'a CommonPools),
}

/// Create a machine with explicit IPv6 loopback intent. The caller's transaction
/// holds the reservation lock and claims IPv4 before committing creation.
pub async fn create_with_requested_loopback(
    txn: &mut PgConnection,
    common_pools: Option<MachineCreationPools<'_>>,
    stable_machine_id: &MachineId,
    state: ManagedHostState,
    expected_machine_data: Option<&ExpectedMachineData>,
    state_model_version: i16,
    requested_loopback_v6: Option<Ipv6Addr>,
) -> DatabaseResult<AnyMachine> {
    let stable_machine_id_string = stable_machine_id.to_string();
    let (common_pools, asn_pool) = match common_pools {
        Some(MachineCreationPools::All(pools)) => (Some(pools), Some(&pools.ethernet.pool_fnn_asn)),
        Some(MachineCreationPools::Ipv6Only(pools)) => (Some(pools), None),
        None => (None, None),
    };

    let default_metadata = &Metadata::default();
    let metadata = expected_machine_data
        .map(|data| &data.metadata)
        .unwrap_or(default_metadata);
    let metadata_name = if !metadata.name.is_empty() {
        &metadata.name
    } else {
        &stable_machine_id_string
    };
    let rack_id = expected_machine_data.and_then(|data| data.rack_id.as_ref());
    let sku_id = expected_machine_data.and_then(|data| data.sku_id.as_ref());
    let dpf_enabled = match expected_machine_data {
        Some(data) => data.dpf_enabled.unwrap_or(true), // EM entry exists
        None => true,                                   // EM entry does not exist
    };
    let host_profile = HostProfile::from_expected_machine(expected_machine_data);
    let spx_multiplane = if stable_machine_id.machine_type() == MachineType::Host {
        expected_machine_data
            .map(|data| MachineSpxMultiplane::from_expected_interfaces(&data.interfaces))
            .transpose()
            .map_err(DatabaseError::internal)?
            .flatten()
    } else {
        None
    };

    // Host and DPU machines are created in same `discover_machine` call. Update same
    // state in both machines.
    let state_version = ConfigVersion::initial();
    let version = ConfigVersion::initial();

    let network_config_version = ConfigVersion::initial();
    let loopback_ip_v6 = if stable_machine_id.machine_type() == MachineType::Dpu {
        match common_pools {
            Some(common_pools) => {
                allocate_loopback_ip_v6(
                    common_pools,
                    txn,
                    &stable_machine_id_string,
                    requested_loopback_v6,
                )
                .await?
            }
            None => None,
        }
    } else {
        None
    };
    let network_config = ManagedHostNetworkConfig {
        loopback_ip_v6,
        ..Default::default()
    };
    let asn: Option<i64> = if stable_machine_id.machine_type() == MachineType::Dpu {
        if let Some(asn_pool) = asn_pool {
            match crate::resource_pool::allocate(
                asn_pool,
                txn,
                resource_pool::OwnerType::Machine,
                &stable_machine_id_string,
                None,
            )
            .await
            {
                Ok(asn) => Some(asn as i64),
                Err(e) => {
                    crate::resource_pool::emit_best_effort_dpu_asn_allocation_failure(
                        asn_pool.value_type,
                        &stable_machine_id_string,
                        &e,
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let query = r#"INSERT INTO machines(
                            id, controller_state_version, controller_state, network_config_version, network_config, machine_state_model_version, asn, version, name, description, labels, rack_id, hw_sku, dpf, host_profile, spx_multiplane)
                            VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::json, $12, $13, $14, $15, $16) RETURNING id"#;
    let machine_id: MachineId = sqlx::query_as(query)
        .bind(&stable_machine_id_string)
        .bind(state_version)
        .bind(sqlx::types::Json(&state))
        .bind(network_config_version)
        .bind(sqlx::types::Json(&network_config))
        .bind(state_model_version)
        .bind(asn)
        .bind(version)
        .bind(metadata_name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(rack_id)
        .bind(sku_id)
        .bind(sqlx::types::Json(Dpf {
            enabled: dpf_enabled,
            used_for_ingestion: false,
        }))
        .bind(sqlx::types::Json(&host_profile))
        .bind(spx_multiplane.as_ref().map(sqlx::types::Json))
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if machine_id != *stable_machine_id {
        return Err(DatabaseError::internal(format!(
            "Machine {stable_machine_id} was just created, but database failed to return any rows"
        )));
    }

    let machine = find_one(&mut *txn, stable_machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: stable_machine_id.to_string(),
        })?;
    advance(&machine, txn, &state, None).await?;

    // Create a entry in power_options table as well.
    if !machine.is_dpu() {
        crate::power_options::create(&machine.id, txn).await?;
    }
    Ok(machine)
}

pub async fn update_slot_and_tray(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    slot_number: Option<i32>,
    tray_index: Option<i32>,
) -> DatabaseResult<()> {
    sqlx::query(
        "UPDATE machines SET slot_number = COALESCE($1, slot_number), \
         tray_index = COALESCE($2, tray_index) WHERE id = $3",
    )
    .bind(slot_number)
    .bind(tray_index)
    .bind(machine_id)
    .execute(txn)
    .await
    .map_err(|e| DatabaseError::new("update_slot_and_tray", e))?;
    Ok(())
}

// Trigger DPU reprovisioning. For machine assigned to user, needs user approval to start
// reprovisioning.
pub async fn trigger_dpu_reprovisioning_request(
    machine_id: &DpuMachineId,
    txn: &mut PgConnection,
    initiator: &str,
    update_firmware: bool,
) -> Result<(), DatabaseError> {
    let reprovision_time = chrono::Utc::now();
    let req = ReprovisionRequest {
        requested_at: reprovision_time,
        initiator: initiator.to_string(),
        update_firmware,
        started_at: None,
        user_approval_received: false,
        restart_reprovision_requested_at: reprovision_time,
    };

    let query = "UPDATE machines SET reprovisioning_requested=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(req))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

// Update reprovision start time to the current time
pub async fn update_dpu_reprovision_start_time(
    machine_id: &DpuMachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    update_dpu_reprovision_explicit_start_time(machine_id, chrono::Utc::now(), txn).await
}

// Update reprovision start time to a specific start time
pub async fn update_dpu_reprovision_explicit_start_time(
    machine_id: &DpuMachineId,
    time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET reprovisioning_requested=
                                    jsonb_set(reprovisioning_requested,
                                                '{started_at}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(time))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

// Update reprovision start time to the current time
pub async fn update_host_reprovision_start_time(
    machine_id: &HostMachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    update_host_reprovision_explicit_start_time(machine_id, chrono::Utc::now(), txn).await
}

pub async fn update_host_reprovision_explicit_start_time(
    machine_id: &HostMachineId,
    time: DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET host_reprovisioning_requested=
                                    jsonb_set(host_reprovisioning_requested,
                                                '{started_at}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(time))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn get_host_reprovisioning_machines(
    txn: &mut PgConnection,
) -> Result<Vec<HostMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{}
                WHERE m.host_reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

pub async fn update_firmware_update_time_window_start_end(
    machine_ids: &[impl MachineIdSubtypeTrait],
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET firmware_update_time_window_start = $2, firmware_update_time_window_end = $3, update_complete = false
                       WHERE id = ANY($1) RETURNING id"#;
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_ids.iter().map(|x| x.to_string()).collect_vec())
        .bind(start)
        .bind(end)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(())
}

pub async fn update_update_complete(
    machine_id: &HostMachineId,
    complete: bool,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET update_complete = $2
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(complete)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(())
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    outcome: PersistentStateHandlerOutcome,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET controller_state_outcome=$1 WHERE id=$2";
    sqlx::query(query)
        .bind(sqlx::types::Json(outcome))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

// Update user's approval status in db.
pub async fn approve_dpu_reprovision_request(
    machine_id: &DpuMachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET reprovisioning_requested=
                                    jsonb_set(reprovisioning_requested,
                                                '{user_approval_received}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(true))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn approve_host_reprovision_request(
    machine_id: &HostMachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET host_reprovisioning_requested=
                                    jsonb_set(host_reprovisioning_requested,
                                                '{user_approval_received}', $2, true)
                       WHERE id=$1 RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(true))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// This will reset the dpu_reprov request.
pub async fn restart_dpu_reprovisioning(
    txn: &mut PgConnection,
    machine_ids: &[&DpuMachineId],
    update_firmware: bool,
) -> Result<(), DatabaseError> {
    let restart_request = ReprovisionRequestRestart {
        update_firmware,
        restart_reprovision_requested_at: chrono::Utc::now(),
    };
    let query = r#"UPDATE machines
                                SET reprovisioning_requested=reprovisioning_requested || $1
                        WHERE id=ANY($2) RETURNING id"#;

    let str_list: Vec<String> = machine_ids.iter().map(|id| id.to_string()).collect();
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(restart_request))
        .bind(str_list)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("restart reprovisioning_requested", e))?;

    Ok(())
}

/// This will fail if reprovisioning is already started.
pub async fn clear_dpu_reprovisioning_request(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    validate_started_time: bool,
) -> Result<(), DatabaseError> {
    let query = if validate_started_time {
        "UPDATE machines SET reprovisioning_requested=NULL
            WHERE id=$1 AND reprovisioning_requested->'started_at' = 'null'::jsonb RETURNING id"
    } else {
        "UPDATE machines SET reprovisioning_requested=NULL
            WHERE id=$1 RETURNING id"
    };

    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("clear reprovisioning_requested", e))?;

    Ok(())
}

pub async fn list_machines_requested_for_reprovisioning(
    txn: impl DbReader<'_>,
) -> Result<Vec<DpuMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

pub async fn list_machines_requested_for_host_reprovisioning(
    txn: impl DbReader<'_>,
) -> Result<Vec<HostMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            "{} WHERE m.host_reprovisioning_requested IS NOT NULL",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

/// Records a reset request, replacing a pending one. Reports false if one has already started.
pub async fn trigger_managed_host_reset_request(
    txn: &mut PgConnection,
    initiator: &str,
    machine_id: &MachineId,
    ignore_cleanup: bool,
) -> Result<bool, DatabaseError> {
    let req = ResetRequest {
        requested_at: chrono::Utc::now(),
        initiator: initiator.to_string(),
        started_at: None,
        ignore_cleanup,
    };

    let query = "UPDATE machines SET reset_requested=$2
                     WHERE id=$1
                       AND (reset_requested IS NULL
                            OR reset_requested->'started_at' = 'null'::jsonb) RETURNING id";
    let requested = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(req))
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(requested.is_some())
}

/// Marks the reset as started, which closes it to `Clear` and stops the hinge re-firing.
/// Guarded on `IS NOT NULL` because `jsonb_set` on a `NULL` column reports success unchanged.
pub async fn update_managed_host_reset_start_time(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = r#"UPDATE machines
                        SET reset_requested=
                                    jsonb_set(reset_requested, '{started_at}', $2, true)
                       WHERE id=$1 AND reset_requested IS NOT NULL RETURNING id"#;
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(chrono::Utc::now()))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Clears a reset request and reports whether a row matched, optionally sparing a started one.
pub async fn clear_managed_host_reset_request(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    only_if_not_started: bool,
) -> Result<bool, DatabaseError> {
    let query = if only_if_not_started {
        "UPDATE machines SET reset_requested=NULL
            WHERE id=$1 AND reset_requested->'started_at' = 'null'::jsonb RETURNING id"
    } else {
        "UPDATE machines SET reset_requested=NULL WHERE id=$1 RETURNING id"
    };

    let cleared = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("clear reset_requested", e))?;

    Ok(cleared.is_some())
}

pub async fn list_machines_requested_for_reset(
    txn: impl DbReader<'_>,
) -> Result<Vec<HostMachine>, DatabaseError> {
    lazy_static! {
        // Oldest first with the id breaking ties, since Postgres guarantees no order otherwise.
        // Cast because the stored text has variable fractional digits and will not sort.
        static ref query: String = format!(
            "{} WHERE m.reset_requested IS NOT NULL
                ORDER BY (m.reset_requested->>'requested_at')::timestamptz, m.id",
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))
}

/// `find_dpu_agent_upgrade_status` reads the current upgrade inputs for an attached DPU.
/// Returns `None` if the DPU, its host, or their non-BMC attachment is absent.
pub async fn find_dpu_agent_upgrade_status<DB>(
    conn: &mut DB,
    dpu_machine_id: &DpuMachineId,
) -> DatabaseResult<Option<DpuAgentUpgradeStatus>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let host_ids =
        DpuHostAssociations::load(&mut *conn, std::slice::from_ref(dpu_machine_id)).await?;
    let Some(host_machine_id) = host_ids.host_machine_id(dpu_machine_id) else {
        return Ok(None);
    };

    // Recheck the attachment in this read, since it can change after the host lookup.
    // EXISTS avoids duplicate rows when several host interfaces attach to the DPU.
    let query = r#"
        SELECT dpu.id,
               dpu.network_status_observation->>'agent_version' AS agent_version,
               COALESCE((dpu.dpu_agent_upgrade_requested->>'should_upgrade')::bool, false)
                   AS upgrade_requested,
               (host.dpf->>'used_for_ingestion')::bool AS host_dpf_used_for_ingestion
        FROM machines dpu
        JOIN machines host ON host.id = $2
        WHERE dpu.id = $1
          AND EXISTS (
              SELECT 1 FROM machine_interfaces mi
              WHERE mi.machine_id = host.id
                AND mi.attached_dpu_machine_id = dpu.id
                AND mi.attached_dpu_machine_id != mi.machine_id
                AND mi.interface_type != 'Bmc'
          )"#;
    sqlx::query_as(query)
        .bind(dpu_machine_id)
        .bind(host_machine_id)
        .fetch_optional(conn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// `apply_agent_upgrade_policy` evaluates the supplied agent version and upgrade request.
/// The caller handles hosts ingested through DPF before calling this helper.
/// An `Off` policy or an absent version leaves the upgrade decision unchanged.
pub async fn apply_agent_upgrade_policy(
    txn: &mut PgConnection,
    policy: AgentUpgradePolicy,
    status: &DpuAgentUpgradeStatus,
) -> Result<(), DatabaseError> {
    if policy == AgentUpgradePolicy::Off {
        return Ok(());
    }

    let Some(agent_version) = status.agent_version.as_deref() else {
        return Ok(());
    };
    let carbide_api_version = carbide_version::v!(build_version);
    let should_upgrade = policy.should_upgrade(agent_version, carbide_api_version);
    if should_upgrade != status.upgrade_requested {
        set_dpu_agent_upgrade_requested(txn, &status.id, should_upgrade, carbide_api_version)
            .await?;
    }

    Ok(())
}

pub async fn set_dpu_agent_upgrade_requested(
    txn: &mut PgConnection,
    machine_id: &DpuMachineId,
    should_upgrade: bool,
    to_version: &str,
) -> Result<(), DatabaseError> {
    let decision = UpgradeDecision {
        should_upgrade,
        to_version: to_version.to_string(),
        last_updated: chrono::Utc::now(),
    };
    let query = "UPDATE machines SET dpu_agent_upgrade_requested = $1::json WHERE id = $2";
    sqlx::query(query)
        .bind(sqlx::types::Json(decision))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn find_machine_ids<ID>(
    txn: impl DbReader<'_>,
    search_config: MachineSearchConfig,
) -> Result<Vec<ID>, DatabaseError>
where
    ID: TryFrom<MachineId>,
    DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    let mut qb = sqlx::QueryBuilder::new("SELECT id FROM machines");

    if search_config.mnnvl_only {
        qb.push(" INNER JOIN machine_topologies mt ON machines.id = mt.machine_id");
    }

    // Return only machines that are powered on and have a health override with leak classification
    if let Some(pstate) = &search_config.only_with_power_state {
        let pstate_normalized = pstate.to_lowercase();
        qb.push(" INNER JOIN power_options po ON po.host_id = machines.id AND po.last_fetched_power_state = ");
        qb.push_bind(pstate_normalized);
        qb.push("::host_power_state_t");
    }

    qb.push(" WHERE TRUE");

    if search_config.only_maintenance {
        qb.push(" AND health_reports->'merges'->'maintenance'->'alerts'->0->>'id' = 'Maintenance'");
    }

    if search_config.only_quarantine {
        qb.push(" AND ");

        // If we're including DPU's, don't filter them out (DPU's don't get the quarantine state,
        // only the managed host does.)
        if search_config.include_dpus {
            qb.push(format!(
                "(starts_with(id, '{}') OR network_config->>'quarantine_state' IS NOT NULL) ",
                MachineType::Dpu.id_prefix(),
            ));
        } else {
            qb.push("network_config->>'quarantine_state' IS NOT NULL ");
        }
    }

    if !search_config.include_dpus {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::Dpu.id_prefix(),
        ));
    }

    if search_config.exclude_hosts {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::Host.id_prefix(),
        ));
    }

    if !search_config.include_predicted_host {
        qb.push(format!(
            " AND NOT starts_with(id, '{}')",
            MachineType::PredictedHost.id_prefix(),
        ));
    }

    if let Some(ovrrd_str) = &search_config.only_with_health_alert {
        // Match the machine_health_source_keys_gin_idx index.
        qb.push(
            " AND jsonb_path_query_array(health_reports, '$.merges.keyvalue().key'::jsonpath) ? ",
        );
        qb.push_bind(ovrrd_str.clone());
        qb.push(" AND jsonb_array_length(health_reports->'merges'->");
        qb.push_bind(ovrrd_str);
        qb.push("->'alerts') > 0");
    }
    if search_config.mnnvl_only {
        qb.push(format!(
            " AND EXISTS (
                SELECT 1
                FROM jsonb_array_elements(
                    COALESCE(mt.topology->'discovery_data'->'Info'->'gpus', '[]'::jsonb)
                ) AS gpu
                WHERE gpu->'platform_info' IS NOT NULL
                  AND gpu->'platform_info' <> 'null'::jsonb
                  AND (
                      {}
                  )
            )",
            mnnvl_gpu_name_sql_like_conditions()
        ));
    }

    if let Some(id) = search_config.instance_type_id {
        qb.push(" AND instance_type_id = ");
        qb.push_bind(id);
    }

    if let Some(rack_id) = search_config.rack_id {
        qb.push(" AND rack_id = ");
        qb.push_bind(rack_id);
    }

    if let Some(state) = search_config.controller_state {
        qb.push(" AND controller_state->>'state' = ");
        qb.push_bind(state);
    }

    if search_config.for_update {
        qb.push(" ORDER BY id ");
        qb.push(" FOR UPDATE");
    }

    let q = qb.build_query_as();

    let machine_ids: Vec<MachineId> = q
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("find_machine_ids", e))?;

    Ok(machine_ids
        .into_iter()
        .map(ID::try_from)
        .collect::<Result<Vec<ID>, _>>()?)
}

/// `try_update_controller_state` updates a host and its attached DPUs only if
/// the host's controller-state version still matches `expected_version`.
///
/// The caller supplies `new_version` for every machine and its history entry.
/// `NotApplied(ControllerStateNotCurrent)` means the host is missing or its
/// version changed, or a captured maintenance request is no longer pending;
/// no state or history changes remain. Successful writes return
/// `Applied(())` and remain in the caller's transaction. Database failures remain
/// errors.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &ManagedHostState,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    let mut inner_txn = Transaction::begin_inner(txn).await?;

    // `advance` takes the history retention lock before the machine row lock.
    // Reversing that order can deadlock. Roll back the history if the host
    // version changed.
    crate::state_history::persist(
        inner_txn.as_pgconn(),
        crate::state_history::StateHistoryTableId::Machine,
        host_id,
        new_state,
        new_version,
    )
    .await?;

    // A replaced or cancelled request cannot be admitted from an old snapshot.
    let maintenance_request = match new_state {
        ManagedHostState::Maintenance { request, .. } => request.as_ref(),
        _ => None,
    };
    let query = r#"
        UPDATE machines
        SET controller_state_version = $1, controller_state = $2
        WHERE id = $3 AND controller_state_version = $4
          AND ($5::jsonb IS NULL OR machine_maintenance_requested = $5)
        RETURNING id
    "#;
    let updated: Option<HostMachineId> = sqlx::query_scalar(query)
        .bind(new_version)
        .bind(sqlx::types::Json(new_state))
        .bind(host_id)
        .bind(expected_version)
        .bind(maintenance_request.map(sqlx::types::Json))
        .fetch_optional(inner_txn.as_pgconn())
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if updated.is_none() {
        inner_txn.rollback().await?;
        return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
    }

    tracing::info!(machine_id = %host_id, next_state = ?new_state, "Updating host state");
    for dpu in find_dpus_by_host_machine_id(inner_txn.as_pgconn(), host_id).await? {
        advance(&dpu, inner_txn.as_pgconn(), new_state, Some(new_version)).await?;
    }
    inner_txn.commit().await?;
    Ok(ConditionalWrite::Applied(()))
}

/// `lock_nic_firmware_state` locks history retention before the host row and
/// checks the snapshot used by firmware reconciliation. A missing host or stale
/// version returns `NotApplied(ControllerStateNotCurrent)` without any writes.
/// Locks remain in the caller's transaction, which must perform its state CAS
/// before committing related writes. Any instance lock must be acquired first.
pub async fn lock_nic_firmware_state(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_version: ConfigVersion,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    // `advance` acquires this advisory lock through the history trigger before
    // locking machines. Side writes in the same transaction must keep that order.
    let query = "SELECT pg_advisory_xact_lock(\
                    hashtextextended($1, 'machine_state_history'::regclass::bigint)\
                )";
    sqlx::query(query)
        .bind(host_id.to_string())
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let query = "SELECT id FROM machines
                 WHERE id = $1 AND controller_state_version = $2 FOR UPDATE";
    let locked: Option<HostMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .bind(expected_version)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(match locked {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    })
}

/// `has_recorded_managed_host_failure` returns true when the host or a DPU
/// attached through any non-BMC host interface has a cause other than
/// `FailureCause::NoError`. BMC-only and unrelated DPU failures are excluded.
/// Malformed failure details, including SQL or JSON null, return database errors.
/// This takes no locks and returns false when no selected row records a failure;
/// callers establish host existence and serialize any decision based on this read.
pub async fn has_recorded_managed_host_failure(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> Result<bool, DatabaseError> {
    let query = "SELECT failure_details FROM machines
                 WHERE id = $1 OR id IN (
                     SELECT attached_dpu_machine_id FROM machine_interfaces
                     WHERE machine_id = $1 AND interface_type != 'Bmc'
                       AND attached_dpu_machine_id IS NOT NULL
                 )";
    let failures: Vec<sqlx::types::Json<FailureDetails>> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(failures
        .iter()
        .any(|details| details.cause != model::machine::FailureCause::NoError))
}

mod host_pfs;
pub use host_pfs::{
    bf4_host_pf_initialization_pending, host_has_bf4_dpu, lock_host_init_network_config,
};

/// Recheck the lifecycle owner under Instance -> History -> Machine locks.
/// Missing or stale hosts, an ineligible named owner, and conflicting
/// attachments or recorded host/DPU failures return
/// `NotApplied(ControllerStateNotCurrent)`. Busy interface or DPU locks also
/// defer completion. Rejection rolls back the helper's savepoint and releases
/// locks acquired inside it.
/// Success retains the owner, host, every host-interface row, and each attached
/// non-BMC DPU's history and row locks in the caller's transaction. The caller
/// must perform its guarded state transition before committing. This helper checks
/// neither firmware evidence nor other readiness and changes no state or history.
pub async fn prepare_nic_firmware_completion(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_version: ConfigVersion,
    purpose: &NicFirmwarePurpose,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    let mut inner_txn = Transaction::begin_inner(txn).await?;
    let expected_instance = match purpose {
        NicFirmwarePurpose::Baseline { .. } => None,
        NicFirmwarePurpose::Allocation { instance_id, .. }
        | NicFirmwarePurpose::RestoreBaseline { instance_id, .. } => Some(*instance_id),
    };
    if let Some(instance_id) = expected_instance {
        // Instance configuration and release take Instance before Machine.
        // Keep that order, followed by the state helper's History before Machine.
        let query = "SELECT machine_id, deleted FROM instances WHERE id = $1 FOR UPDATE";
        let owner: Option<(HostMachineId, Option<DateTime<Utc>>)> = sqlx::query_as(query)
            .bind(instance_id)
            .fetch_optional(inner_txn.as_pgconn())
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        let owner_matches = match owner {
            Some((machine_id, deleted)) => {
                machine_id == *host_id
                    && purpose.matches_owner(Some((instance_id, deleted.is_some())))
            }
            None => purpose.matches_owner(None),
        };
        if !owner_matches {
            inner_txn.rollback().await?;
            return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
        }
    }
    let applied = lock_nic_firmware_state(inner_txn.as_pgconn(), host_id, expected_version).await?;
    if let ConditionalWrite::NotApplied(reason) = applied {
        inner_txn.rollback().await?;
        return Ok(ConditionalWrite::NotApplied(reason));
    }
    // Allocation locks the host before inserting its Instance. Holding that row
    // now also protects an absent owner from a new assignment until commit.
    let query = "SELECT id FROM instances WHERE machine_id = $1";
    let attached_instance: Option<InstanceId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_optional(inner_txn.as_pgconn())
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let attachment_matches = match purpose {
        NicFirmwarePurpose::Baseline { .. } => attached_instance.is_none(),
        NicFirmwarePurpose::Allocation { .. } => attached_instance == expected_instance,
        NicFirmwarePurpose::RestoreBaseline { .. } => {
            attached_instance.is_none() || attached_instance == expected_instance
        }
    };
    if !attachment_matches {
        inner_txn.rollback().await?;
        return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
    }

    if !lock_nic_firmware_failure_state(inner_txn.as_pgconn(), host_id).await? {
        inner_txn.rollback().await?;
        return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
    }
    inner_txn.commit().await?;
    Ok(ConditionalWrite::Applied(()))
}

// The caller already holds the host history and row locks. True means every
// host-interface row and attached non-BMC DPU history/row lock is held, and the
// selected machines record no failure. False means a dependency was busy or
// changed, or a selected machine records a failure. Both results retain acquired
// locks; completion rolls its savepoint back on false, while owner release leaves
// them with its caller. Never wait behind writers that take a DPU before the host.
async fn lock_nic_firmware_failure_state(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> Result<bool, DatabaseError> {
    Ok(try_lock_host_interfaces_and_dpus(txn, host_id).await?
        && !has_recorded_managed_host_failure(txn, host_id).await?)
}

/// Tries interface and DPU history/row locks after the host lock without waiting behind interface-first writers.
///
/// Callers must hold the host history and machine locks and roll back after a false result.
async fn try_lock_host_interfaces_and_dpus(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
) -> Result<bool, DatabaseError> {
    // Freeze existing interfaces before choosing the DPU set, including BMC
    // rows that could become data interfaces. New host references wait on the
    // host lock. Discovery takes interfaces before machines, so never wait here.
    let query = "SELECT id FROM machine_interfaces WHERE machine_id = $1 ORDER BY id";
    let expected_interfaces: Vec<MachineInterfaceId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    let query = "SELECT id FROM machine_interfaces WHERE machine_id = $1
                 ORDER BY id FOR UPDATE SKIP LOCKED";
    let locked_interfaces: Vec<MachineInterfaceId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if locked_interfaces != expected_interfaces {
        return Ok(false);
    }

    let query = "SELECT id FROM machines WHERE id IN (
                     SELECT attached_dpu_machine_id FROM machine_interfaces
                     WHERE machine_id = $1 AND interface_type != 'Bmc'
                       AND attached_dpu_machine_id IS NOT NULL
                 ) ORDER BY id";
    let dpu_ids: Vec<DpuMachineId> = sqlx::query_scalar(query)
        .bind(host_id)
        .fetch_all(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    for dpu_id in dpu_ids {
        // DPU network writers take their row before the host. Try both locks
        // without waiting; the later CAS still takes history before the row.
        let history_locked = try_lock_machine_state_history(&mut *txn, &dpu_id.into()).await?;
        if !history_locked || !try_lock_by_id_for_update(&mut *txn, &dpu_id.into()).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `prepare_nic_firmware_owner_release` checks that the original allocation is
/// still attached and deleted before returning safely restored firmware work to
/// normal instance release. Configuration changes do not matter for that deleted
/// owner. Missing, live or replaced owners, stale host versions, recorded
/// host/DPU failures, and busy dependency locks return
/// `NotApplied(ControllerStateNotCurrent)`.
///
/// The caller must have completed lockdown restoration; this checks ownership
/// and recorded failures, not firmware evidence.
/// This helper writes no state or history. Its Instance → History → Machine
/// locks and acquired interface/DPU locks remain in the caller's transaction,
/// including on `NotApplied`. A caller that proceeds must perform its state CAS
/// before committing that transaction.
pub async fn prepare_nic_firmware_owner_release(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    expected_version: ConfigVersion,
    instance_id: &InstanceId,
) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
    let query = "SELECT id FROM instances
                 WHERE id = $1 AND machine_id = $2 AND deleted IS NOT NULL FOR UPDATE";
    let owner: Option<InstanceId> = sqlx::query_scalar(query)
        .bind(instance_id)
        .bind(host_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if owner.is_none() {
        return Ok(ConditionalWrite::NotApplied(ControllerStateNotCurrent));
    }
    // `instances_unique_machine_id` permits only this attached owner. Holding
    // its row prevents removal or reassignment before the caller's transition.
    if let ConditionalWrite::NotApplied(reason) =
        lock_nic_firmware_state(txn, host_id, expected_version).await?
    {
        return Ok(ConditionalWrite::NotApplied(reason));
    }
    Ok(if lock_nic_firmware_failure_state(txn, host_id).await? {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(ControllerStateNotCurrent)
    })
}

pub async fn update_state(
    txn: &mut PgConnection,
    host_id: &HostMachineId,
    new_state: &ManagedHostState,
) -> Result<(), DatabaseError> {
    let host = find_one(
        &mut *txn,
        host_id,
        // TODO(?): Should we be using for_update/row-level locks here?
        // This is a select that's later used for an update on both version
        // and state below with the calls to `advance`.
        crate::machine::MachineSearchConfig::default(),
    )
    .await?
    .ok_or_else(|| DatabaseError::new("crate::machine::find_one", sqlx::Error::RowNotFound))?;

    let version = host.current_version().increment();
    tracing::info!(machine_id = %host.id, next_state = ?new_state, "Updating host state");
    advance(&host, txn, new_state, Some(version)).await?;

    // Keep both host and dpu's states in sync.
    let dpus = find_dpus_by_host_machine_id(txn, host_id).await?;

    for dpu in dpus {
        advance(&dpu, txn, new_state, Some(version)).await?;
    }
    Ok(())
}

/// Records the database statement execution time as the machine's validation timestamp.
///
/// This ensures validation completed after a concurrent state transition is recorded as newer than
/// that transition, even when the validation transaction began first.
pub async fn update_machine_validation_time(
    machine_id: &MachineId,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET last_machine_validation_time=clock_timestamp() WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_machine_validation_id(
    machine_id: &MachineId,
    validation_id: MachineValidationId,
    context: MachineValidationContext,
    txn: &mut PgConnection,
) -> Result<MachineId, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("UPDATE machines SET ");
    let query = builder
        .push({
            match context {
                MachineValidationContext::Discovery => "discovery_machine_validation_id",
                MachineValidationContext::Cleanup => "cleanup_machine_validation_id",
                MachineValidationContext::OnDemand => "on_demand_machine_validation_id",
            }
        })
        .push("=")
        .push_bind(validation_id)
        .push(" WHERE id=")
        .push_bind(machine_id)
        .push(" RETURNING id");

    query
        .build_query_as()
        .bind(validation_id)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query.sql(), e))
}

pub async fn update_failure_details_by_machine_id(
    machine_id: &MachineId,
    txn: &mut PgConnection,
    failure: FailureDetails,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET failure_details = $1::json WHERE id = $2 RETURNING id";
    let _id: (MachineId,) = sqlx::query_as(query)
        .bind(sqlx::types::Json(failure))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

/// Find a list of DPU operational information.
///
/// Returns: `Vec<DpuInfo>` - A list of DPU information with loopback and operational state.
///
/// Arguments
///
/// * `txn` - A reference to currently open database transaction
pub async fn find_dpu_infos(txn: &mut PgConnection) -> Result<Vec<DpuInfo>, DatabaseError> {
    type DpuInfoRow = (
        String,
        String,
        sqlx::types::Json<ManagedHostState>,
        sqlx::types::Json<Option<MachineNetworkStatusObservation>>,
        Option<String>,
    );

    // Pull the DPU rows with the JSON fields needed to shape the response.
    let query = r#"
        SELECT
            m.id,
            m.network_config->>'loopback_ip' AS loopback_ip,
            m.controller_state,
            COALESCE(m.network_status_observation, 'null'::jsonb) AS network_status_observation,
            mt.topology->'discovery_data'->'Info'->'dpu_info'->>'firmware_version' AS firmware_version
        FROM machines m
        LEFT JOIN machine_topologies mt ON mt.machine_id = m.id
        WHERE m.network_config->>'loopback_ip' IS NOT NULL
        ORDER BY m.id"#;

    let dpu_rows: Vec<DpuInfoRow> = sqlx::query_as(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // TODO: Replace this heuristic once the agent reports interface roles explicitly.
    let is_representor = |interface_name: &str| interface_name.starts_with("pf");

    let representor_statuses = |interfaces: &[DpuFabricInterfaceStatusObservation]| {
        let mut representors: Vec<_> = interfaces
            .iter()
            .filter(|interface| is_representor(&interface.interface_name))
            .map(|interface| DpuRepresentorStatus {
                name: interface.interface_name.clone(),
                carrier_up: interface
                    .link_data
                    .as_ref()
                    .and_then(|link| link.carrier_up),
                state: interface
                    .link_data
                    .as_ref()
                    .and_then(|link| link.state.clone()),
            })
            .collect();

        representors.sort_by(|left, right| left.name.cmp(&right.name));
        representors
    };

    // Shape each DB row into the public DPU info model.
    dpu_rows
        .into_iter()
        .map(
            |(id, loopback_ip, controller_state, network_status_observation, firmware_version)| {
                let dpu_id = DpuMachineId::try_from(MachineId::from_str(&id).map_err(|e| {
                    DatabaseError::internal(format!("Invalid DPU machine ID {id}: {e}"))
                })?)
                .map_err(|e| DatabaseError::internal(e.to_string()))?;
                let network_status_observation = network_status_observation.0;
                let representors = network_status_observation
                    .as_ref()
                    .map(|observation| representor_statuses(&observation.fabric_interfaces))
                    .unwrap_or_default();

                Ok(DpuInfo {
                    id,
                    loopback_ip,
                    observed_status: Some(DpuInfoStatusObservation {
                        os_operational_state: Some(DpuOsOperationalState {
                            state_detail: controller_state.0.dpu_state_string(&dpu_id),
                        }),
                        firmware_version,
                        representors,
                        last_heartbeat: network_status_observation
                            .as_ref()
                            .map(|observation| observation.observed_at),
                    }),
                })
            },
        )
        .collect()
}

/// Allocates one IPv6 underlay loopback when `lo-ip-v6` is configured.
/// An absent pool yields `None`, preserving IPv4-only sites; exhaustion remains
/// an error so a configured site cannot silently leave a DPU without its
/// address. `requested_value` selects a specific operator-reserved address from
/// the non-auto-assign partition, while `None` keeps the automatic assignment;
/// a value already owned by this DPU is reused either way.
pub async fn allocate_loopback_ip_v6(
    common_pools: &CommonPools,
    txn: &mut PgConnection,
    owner_id: &str,
    requested_value: Option<Ipv6Addr>,
) -> Result<Option<Ipv6Addr>, DatabaseError> {
    let pool = &common_pools.ethernet.pool_loopback_ip_v6;
    if !crate::resource_pool::pool_has_rows(txn, pool.name()).await? {
        if requested_value.is_some() {
            return Err(DatabaseError::FailedPrecondition(
                "requested IPv6 loopback requires a configured lo-ip-v6 pool".to_string(),
            ));
        }
        return Ok(None);
    }

    if let Some(value) = crate::resource_pool::find_owned_allocation(
        pool,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
    )
    .await?
    {
        return Ok(Some(value));
    }

    match crate::resource_pool::allocate(
        pool,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
        requested_value,
    )
    .await
    {
        Ok(value) => Ok(Some(value)),
        Err(
            error @ crate::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                ResourcePoolError::Empty,
            ),
        ) => {
            crate::resource_pool::emit_allocation_failure(
                pool.value_type,
                owner_id,
                false,
                LOOPBACK_IP_V6,
                &error,
            );
            Err(DatabaseError::ResourceExhausted(format!(
                "pool {LOOPBACK_IP_V6}"
            )))
        }
        Err(error) => {
            crate::resource_pool::emit_allocation_failure(
                pool.value_type,
                owner_id,
                false,
                LOOPBACK_IP_V6,
                &error,
            );
            Err(error.into())
        }
    }
}

/// Allocate a value from the loopback IP resource pool.
///
/// If the pool exists but is empty or has en error, return that.
pub async fn allocate_vpc_dpu_loopback(
    common_pools: &CommonPools,
    txn: &mut PgConnection,
    owner_id: &str,
) -> Result<IpAddr, DatabaseError> {
    match crate::resource_pool::allocate(
        &common_pools.ethernet.pool_vpc_dpu_loopback_ip,
        txn,
        resource_pool::OwnerType::Machine,
        owner_id,
        None,
    )
    .await
    {
        Ok(val) => Ok(val),
        Err(
            error @ crate::resource_pool::ResourcePoolDatabaseError::ResourcePool(
                resource_pool::ResourcePoolError::Empty,
            ),
        ) => {
            crate::resource_pool::emit_allocation_failure(
                common_pools.ethernet.pool_vpc_dpu_loopback_ip.value_type,
                owner_id,
                false,
                "vpc-dpu-lo-ip",
                &error,
            );
            Err(DatabaseError::ResourceExhausted(
                "pool vpc-dpu-lo-ip".to_string(),
            ))
        }
        Err(err) => {
            crate::resource_pool::emit_allocation_failure(
                common_pools.ethernet.pool_vpc_dpu_loopback_ip.value_type,
                owner_id,
                false,
                "vpc-dpu-lo-ip",
                &err,
            );
            Err(err.into())
        }
    }
}

pub async fn find_by_validation_id(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
) -> Result<Option<AnyMachine>, DatabaseError> {
    lazy_static! {
        static ref query: String = format!(
            r#"{}
                WHERE m.discovery_machine_validation_id = $1
                OR m.cleanup_machine_validation_id = $1
                OR m.on_demand_machine_validation_id = $1"#,
            JSON_MACHINE_SNAPSHOT_QUERY.deref()
        );
    }
    let machine = sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(validation_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query.as_str(), e))?;

    Ok(machine)
}

/// set_firmware_autoupdate flags a machine ID as explicitly having firmware upgrade enabled or disabled, or use config files if None.
pub async fn set_firmware_autoupdate(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    state: Option<bool>,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET firmware_autoupdate = $1 WHERE id = $2";
    sqlx::query(query)
        .bind(state)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn update_rack_fw_details(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    details: Option<&model::rack::RackFirmwareUpgradeStatus>,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET rack_fw_details = $1, updated = NOW() WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, HostMachineId>(query)
        .bind(details.map(|d| sqlx::types::Json(d.clone())))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub async fn set_machine_validation_request(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    machine_validation_request: bool,
) -> Result<(), DatabaseError> {
    let query =
        "UPDATE machines SET on_demand_machine_validation_request=$2 WHERE id=$1 RETURNING id";
    let _id = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(machine_validation_request)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn set_machine_maintenance_requested(
    txn: &mut PgConnection,
    machine_id: impl MachineIdSubtypeTrait,
    initiator: &str,
    operation: MachineMaintenanceOperation,
) -> DatabaseResult<()> {
    let req = model::machine::MachineMaintenanceRequest {
        requested_at: Utc::now(),
        initiator: initiator.to_string(),
        operation,
    };
    let query = "UPDATE machines SET machine_maintenance_requested = $1 WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(req))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("set_machine_maintenance_requested", e))?;
    Ok(())
}

/// Claims maintenance on an unallocated machine at a maintenance admission boundary.
///
/// The caller must use a transaction. The machine row lock serializes with instance
/// allocation; eligibility is rechecked after acquiring it. Returns `false` when
/// the machine is missing or ineligible, without replacing an existing request.
pub async fn try_set_machine_maintenance_requested(
    txn: &mut PgConnection,
    machine_id: impl MachineIdSubtypeTrait,
    initiator: &str,
    operation: MachineMaintenanceOperation,
) -> DatabaseResult<bool> {
    let lock_query = "SELECT controller_state FROM machines WHERE id = $1 FOR UPDATE";
    let state: Option<sqlx::types::Json<ManagedHostState>> = sqlx::query_scalar(lock_query)
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(lock_query, error))?;
    let Some(state) = state.filter(|state| state.accepts_maintenance_request()) else {
        return Ok(false);
    };
    // A separate statement observes an allocation committed while the lock was awaited.
    let request = model::machine::MachineMaintenanceRequest {
        requested_at: Utc::now(),
        initiator: initiator.to_string(),
        operation,
    };
    let query = r#"UPDATE machines SET machine_maintenance_requested = $1
        WHERE id = $2 AND machine_maintenance_requested IS NULL
          AND NOT EXISTS (SELECT 1 FROM instances WHERE machine_id = $2)
        RETURNING id"#;
    let claimed = sqlx::query_as::<_, MachineId>(query)
        .bind(sqlx::types::Json(&request))
        .bind(machine_id)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::new("try_set_machine_maintenance_requested", error))?
        .is_some();
    if claimed {
        tracing::info!(
            machine_id = %machine_id.as_machine_id(),
            initiator,
            operation = ?request.operation,
            previous_state = ?state.0,
            "Queueing machine maintenance request"
        );
    }
    Ok(claimed)
}

/// Cancels only an unstarted Reboot or Reset owned by the rack initiator.
/// Requests already claimed by a machine controller or owned by others are preserved.
pub async fn clear_unclaimed_machine_restart_request(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    initiator: &str,
) -> DatabaseResult<bool> {
    // The conditional update serializes with the transition into Maintenance.
    // A pending request can still be cancelled if another workflow changed the
    // admission state before the machine controller claimed it.
    let query = r#"UPDATE machines SET machine_maintenance_requested = NULL
        WHERE id = $1 AND controller_state->>'state' <> 'maintenance'
          AND machine_maintenance_requested->>'initiator' = $2
          AND machine_maintenance_requested->'operation' IN ($3, $4, $5)
        RETURNING id"#;
    Ok(sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(initiator)
        .bind(sqlx::types::Json(MachineMaintenanceOperation::Reset {
            graceful: false,
        }))
        .bind(sqlx::types::Json(MachineMaintenanceOperation::Reset {
            graceful: true,
        }))
        .bind(sqlx::types::Json(MachineMaintenanceOperation::Reboot))
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .is_some())
}

pub async fn set_decommission_requested(
    txn: &mut PgConnection,
    machine_id: impl MachineIdSubtypeTrait,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET decommission_requested = TRUE WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::new("set_decommission_requested", error))
}

pub async fn clear_decommission_requested(
    txn: &mut PgConnection,
    machine_id: impl MachineIdSubtypeTrait,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET decommission_requested = FALSE WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::new("clear_decommission_requested", error))
}

/// Clears only the maintenance request that the controller completed.
/// A missing machine or a different pending request returns `NotApplied`.
pub async fn clear_machine_maintenance_requested(
    txn: &mut PgConnection,
    machine_id: impl MachineIdSubtypeTrait,
    request: &model::machine::MachineMaintenanceRequest,
) -> DatabaseResult<ConditionalWrite<(), crate::MaintenanceRequestNotCurrent>> {
    let query = "UPDATE machines SET machine_maintenance_requested = NULL WHERE id = $1 AND machine_maintenance_requested = $2 RETURNING id";
    let cleared = sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .bind(sqlx::types::Json(request))
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new("clear_machine_maintenance_requested", e))?;
    Ok(match cleared {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(crate::MaintenanceRequestNotCurrent),
    })
}

/// Record an operator "force-converge this BMC now" request on the machine that
/// owns the BMC (a host machine for its host BMC, a DPU machine for its DPU BMC).
/// The machine state controller consumes it on its next sweep.
pub async fn set_bmc_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<()> {
    let query =
        "UPDATE machines SET bmc_credential_rotation_requested = true WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("set_bmc_credential_rotation_requested", e),
        })?;
    Ok(())
}

pub async fn clear_bmc_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> DatabaseResult<()> {
    let query =
        "UPDATE machines SET bmc_credential_rotation_requested = false WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("clear_bmc_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Record an operator "force-converge this UEFI credential now" request on the
/// machine that owns the UEFI credential (a host machine for its host UEFI; a
/// DPU machine for its DPU UEFI). The machine state controller consumes it on
/// its next sweep. Mirrors [`set_bmc_credential_rotation_requested`].
pub async fn set_uefi_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<()> {
    let query =
        "UPDATE machines SET uefi_credential_rotation_requested = true WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("set_uefi_credential_rotation_requested", e),
        })?;
    Ok(())
}

pub async fn clear_uefi_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: MachineId,
) -> DatabaseResult<()> {
    let query =
        "UPDATE machines SET uefi_credential_rotation_requested = false WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("clear_uefi_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Record that an operator has requested rotating the NIC lockdown keys for a host
/// (bypasses the site-config flag for NIC lockdown rotation)
pub async fn set_lockdown_ikm_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: HostMachineId,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET lockdown_ikm_credential_rotation_requested = true \
                 WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("set_lockdown_ikm_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Clear the force NIC lockdown rotation flag for this host
pub async fn clear_lockdown_ikm_credential_rotation_requested(
    txn: &mut PgConnection,
    machine_id: HostMachineId,
) -> DatabaseResult<()> {
    let query = "UPDATE machines SET lockdown_ikm_credential_rotation_requested = false \
                 WHERE id = $1 RETURNING id";
    sqlx::query_as::<_, MachineId>(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| match e {
            // `RETURNING id` yields no row for an unknown machine; surface a
            // clean not-found rather than a generic wrapped error.
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("clear_lockdown_ikm_credential_rotation_requested", e),
        })?;
    Ok(())
}

/// Read a host's one-shot NIC lockdown IKM rotation force flag. The SVPC scout
/// handler consults this so the tenant-allocation lock migrates a force-flagged
/// host's cards to the site-wide target even when the site-wide gate is off,
/// mirroring the idle rekey path.
pub async fn get_lockdown_ikm_credential_rotation_requested(
    conn: &mut PgConnection,
    machine_id: HostMachineId,
) -> DatabaseResult<bool> {
    let query = "SELECT lockdown_ikm_credential_rotation_requested FROM machines WHERE id = $1";
    sqlx::query_scalar::<_, bool>(query)
        .bind(machine_id)
        .fetch_one(conn)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => DatabaseError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            },
            e => DatabaseError::new("get_lockdown_ikm_credential_rotation_requested", e),
        })
}

/// `get_asn` reads a machine's optional ASN, returning an error for a missing row.
pub async fn get_asn(
    database: impl DbReader<'_>,
    machine_id: &MachineId,
) -> DatabaseResult<Option<u32>> {
    let query = "SELECT asn FROM machines WHERE id = $1";
    let asn: Option<i64> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_one(database)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    asn.map(|asn| u32::try_from(asn).map_err(|error| DatabaseError::internal(error.to_string())))
        .transpose()
}

/// The ASN target is missing, replaced, already assigned or marked `ForceDeletion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineAsnNotCurrent;

/// `try_update_asn_for_created` fills only an absent ASN on the captured live row.
/// Missing, replaced, `ForceDeletion` and already assigned rows return `NotApplied`.
/// Roll back the associated allocation when this conditional write loses.
pub async fn try_update_asn_for_created(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    created: DateTime<Utc>,
    asn: u32,
) -> DatabaseResult<ConditionalWrite<(), MachineAsnNotCurrent>> {
    let query = "UPDATE machines SET asn = $1 WHERE id = $2 AND created = $3
        AND asn IS NULL AND controller_state != $4::jsonb";
    let updated = sqlx::query(query)
        .bind(i64::from(asn))
        .bind(machine_id)
        .bind(created)
        .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?
        .rows_affected();
    Ok(if updated == 0 {
        ConditionalWrite::NotApplied(MachineAsnNotCurrent)
    } else {
        ConditionalWrite::Applied(())
    })
}

/// `find_dpu_ids_without_asn` lists the DPU rows eligible for ASN backfill.
/// A later assignment must recheck the row's lifetime, ASN and deletion state.
pub async fn find_dpu_ids_without_asn(
    database: impl DbReader<'_>,
) -> DatabaseResult<Vec<MachineId>> {
    let query = "SELECT id FROM machines WHERE starts_with(id, $1) AND asn IS NULL
        AND controller_state != $2::jsonb ORDER BY id";
    sqlx::query_as(query)
        .bind(MachineType::Dpu.id_prefix())
        .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
        .fetch_all(database)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `update_dpu_asns` backfills missing DPU ASNs. If an assignment no longer
/// applies, its unused reservation is rolled back and the batch continues.
/// Successful assignments commit together; allocation or database errors roll
/// back the batch.
pub async fn update_dpu_asns(
    db_pool: &Pool<Postgres>,
    common_pools: &CommonPools,
) -> Result<(), DatabaseError> {
    let mut txn = Transaction::begin(db_pool).await?;

    if crate::resource_pool::stats(txn.as_pgconn(), common_pools.ethernet.pool_fnn_asn.name())
        .await?
        .free
        == 0
    {
        tracing::info!(
            "Skipping update of DPU ASNs.  FNN ASN pool not configured or fully allocated"
        );
        return Ok(());
    }
    let query = "SELECT id FROM machines WHERE starts_with(id, $1) AND asn IS NULL";

    let dpu_ids: Vec<MachineId> = sqlx::query_as(query)
        .bind(MachineType::Dpu.id_prefix())
        .fetch_all(txn.as_pgconn())
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    if !dpu_ids.is_empty() {
        tracing::info!(dpu_count = dpu_ids.len(), "Updating missing ASN of DPUs");
    }

    for dpu_machine_id in dpu_ids.iter() {
        let mut assignment_txn = Transaction::begin_inner(txn.as_pgconn()).await?;
        let asn: i64 = crate::resource_pool::allocate(
            &common_pools.ethernet.pool_fnn_asn,
            assignment_txn.as_pgconn(),
            resource_pool::OwnerType::Machine,
            &dpu_machine_id.to_string(),
            None,
        )
        .await? as i64;

        let query = "UPDATE machines set asn=$1 WHERE id=$2 and asn is null";

        let rows_affected = sqlx::query(query)
            .bind(asn)
            .bind(dpu_machine_id)
            .execute(assignment_txn.as_pgconn())
            .await
            .map_err(|e| DatabaseError::query(query, e))?
            .rows_affected();

        if rows_affected == 0 {
            // Another writer assigned the ASN or removed the DPU after our
            // scan. We no longer need the reservation made in this savepoint.
            assignment_txn.rollback().await?;
        } else {
            assignment_txn.commit().await?;
        }
    }

    txn.commit().await?;

    Ok(())
}

/// Fills the optional IPv6 loopback for DPUs created before `lo-ip-v6` was
/// configured. Each DPU is reloaded immediately before its write because an
/// update to one member bumps every sibling's `network_config_version` and
/// invalidates versions loaded earlier in the pass.
///
/// Each DPU gets at most three fresh transactions without backoff. Visible
/// ForceDeletion is skipped before reserving; target/group contention rolls
/// back the reservation and retries without waiting for Machine locks. Earlier
/// DPU commits survive, and later independent DPUs can still progress. Return
/// the first unsettled DPU's last lock/version error after the pass; other
/// errors return immediately. Pool allocation can still wait.
///
/// Startup logs failures without scheduling another pass. A later startup,
/// pool-growth request, or DPU rediscovery can fill a remaining loopback.
pub async fn update_dpu_loopback_ips_v6(
    db_pool: &Pool<Postgres>,
    common_pools: &CommonPools,
) -> Result<(), DatabaseError> {
    const MAX_UPDATE_ATTEMPTS: usize = 3;

    let mut txn = Transaction::begin(db_pool).await?;
    let pool = &common_pools.ethernet.pool_loopback_ip_v6;
    if !crate::resource_pool::pool_has_rows(txn.as_pgconn(), pool.name()).await? {
        txn.commit().await?;
        return Ok(());
    }

    let query = "SELECT id FROM machines
        WHERE starts_with(id, $1)
          AND network_config->>'loopback_ip_v6' IS NULL
        ORDER BY id";
    let dpu_machine_ids: Vec<MachineId> = sqlx::query_as(query)
        .bind(MachineType::Dpu.id_prefix())
        .fetch_all(txn.as_pgconn())
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    txn.commit().await?;

    if !dpu_machine_ids.is_empty() {
        tracing::info!(
            dpu_count = dpu_machine_ids.len(),
            "Updating missing IPv6 loopback IPs for DPUs"
        );
    }

    let mut first_failure = None;

    for dpu_machine_id in dpu_machine_ids {
        let mut last_failure = None;

        for _ in 0..MAX_UPDATE_ATTEMPTS {
            let mut txn = Transaction::begin(db_pool).await?;
            let (mut network_config, network_config_version) =
                match get_network_config(txn.as_pgconn(), &dpu_machine_id).await {
                    Ok(network_config) => network_config.take(),
                    Err(error) if error.is_not_found() => {
                        // Force deletion can remove a DPU after the initial ID
                        // scan. It no longer needs a reservation.
                        txn.commit().await?;
                        last_failure = None;
                        break;
                    }
                    Err(error) => return Err(error),
                };
            if network_config.loopback_ip_v6.is_some() {
                txn.commit().await?;
                last_failure = None;
                break;
            }

            // A committed deletion needs no reservation, even when its row is
            // busy or the pool is exhausted. This read takes no Machine lock.
            if is_force_deleting_or_deleted(txn.as_pgconn(), &dpu_machine_id).await? {
                txn.commit().await?;
                last_failure = None;
                break;
            }

            let loopback_ip_v6 = allocate_loopback_ip_v6(
                common_pools,
                txn.as_pgconn(),
                &dpu_machine_id.to_string(),
                None,
            )
            .await?
            .ok_or_else(|| {
                DatabaseError::FailedPrecondition(format!(
                    "resource pool {LOOPBACK_IP_V6} disappeared during DPU IPv6 loopback backfill"
                ))
            })?;
            network_config.loopback_ip_v6 = Some(loopback_ip_v6);

            // This attempt already owns its reservation. Discovery can own
            // the group before allocating, so release the reservation rather
            // than wait for a busy target that might need the same pool row.
            let query = "SELECT id FROM machines
                         WHERE id = $1 AND controller_state != $2::jsonb
                         FOR NO KEY UPDATE NOWAIT";
            let admitted: Result<Option<MachineId>, DatabaseError> = sqlx::query_scalar(query)
                .bind(dpu_machine_id)
                .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
                .fetch_optional(txn.as_pgconn())
                .await
                .map_err(|error| DatabaseError::query(query, error));
            match admitted {
                Ok(Some(_)) => {}
                Ok(None) => {
                    txn.rollback().await?;
                    last_failure = None;
                    break;
                }
                Err(error) if error.is_lock_not_available() => {
                    txn.rollback().await?;
                    last_failure = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            }

            match try_update_network_config_unless_force_deleting(
                txn.as_pgconn(),
                &dpu_machine_id,
                network_config_version,
                &network_config,
            )
            .await
            {
                Ok(ConditionalWrite::Applied(())) => {
                    txn.commit().await?;
                    last_failure = None;
                    break;
                }
                Ok(ConditionalWrite::NotApplied(MachineNetworkConfigNotCurrent)) => {}
                Err(error) if error.is_lock_not_available() => {
                    // `NOWAIT` aborts the transaction, including its reservation.
                    // Roll it back before reloading the DPU in a fresh attempt.
                    txn.rollback().await?;
                    last_failure = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            }

            txn.rollback().await?;
            last_failure = Some(DatabaseError::ConcurrentModificationError(
                "machine",
                network_config_version.to_string(),
            ));
        }

        if let Some(error) = last_failure {
            // One busy DPU should not postpone every row after it in the scan.
            // Remember the first failure, continue the pass, and report it
            // once the remaining DPUs have had their own chance to reconcile.
            first_failure.get_or_insert(error);
        }
    }

    if let Some(error) = first_failure {
        return Err(error);
    }

    Ok(())
}

pub async fn assign_sku(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    sku_id: &str,
) -> Result<MachineId, DatabaseError> {
    let query = "UPDATE machines SET hw_sku=$1 WHERE id=$2 RETURNING id";

    let id = sqlx::query_as(query)
        .bind(sku_id)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("assign sku to machine", e))?;

    Ok(id)
}

pub async fn unassign_sku(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<MachineId, DatabaseError> {
    let query = "UPDATE machines SET hw_sku=NULL WHERE id=$1 RETURNING id";

    let id = sqlx::query_as(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("assign sku to machine", e))?;

    Ok(id)
}

pub async fn update_sku_status_last_match_attempt(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{last_match_attempt}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku last match attempt", e))?;

    Ok(())
}

pub async fn update_sku_status_last_generate_attempt(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{last_generate_attempt}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku last generate attempt", e))?;

    Ok(())
}

pub async fn update_sku_status_verify_request_time(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{verify_request_time}', $1) WHERE id=$2 RETURNING id";

    sqlx::query_as::<_, ()>(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku status", e))?;

    Ok(())
}

pub async fn update_sku_status_verify_request_time_for_sku(
    txn: &mut PgConnection,
    sku_id: &str,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET hw_sku_status=jsonb_set(coalesce(hw_sku_status, '{}'), '{verify_request_time}', $1) WHERE hw_sku=$2 RETURNING id";

    let ids: Vec<MachineId> = sqlx::query_as(query)
        .bind(sqlx::types::Json(Utc::now()))
        .bind(sku_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("update sku status", e))?;

    tracing::info!(machine_ids=?ids, "SKU updated, requesting verify for affected machines");
    Ok(())
}

pub async fn find_machine_ids_by_sku_ids(
    db_reader: impl DbReader<'_>,
    sku_ids: &[String],
) -> Result<HashMap<String, Vec<MachineId>>, DatabaseError> {
    let query = r#"SELECT skus.id, COALESCE(m.machine_ids, '[]'::jsonb) as associated_machine_ids
  FROM machine_skus AS skus
  LEFT JOIN LATERAL (
    SELECT jsonb_agg(m.id) AS machine_ids
    FROM machines AS m
    WHERE m.hw_sku = skus.id
  ) AS m ON TRUE
  WHERE skus.id=ANY($1)"#;

    #[derive(FromRow)]
    struct Result {
        id: String,
        associated_machine_ids: sqlx::types::Json<Vec<String>>,
    }

    let ids: Vec<Result> = sqlx::query_as(query)
        .bind(sku_ids)
        .fetch_all(db_reader)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    // Build a HashMap of Sku ID to Vec<MachineId>
    Ok(ids
        .into_iter()
        .map(|result| {
            (
                result.id,
                result
                    .associated_machine_ids
                    .0
                    .into_iter()
                    .filter_map(|machine_id_str| machine_id_str.parse().ok())
                    .collect(),
            )
        })
        .collect())
}

/// Records whether force deletion was asked to wait for an Admin acknowledgement.
/// A true requirement survives retries; false only preserves an existing choice
/// or records that no wait was requested. The caller must hold the host row lock
/// and commit this together with `ForceDeletion` and any Admin request.
pub async fn record_force_delete_admin_ack_requirement(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    requires_admin_ack: bool,
) -> DatabaseResult<bool> {
    let query = "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1";
    let recorded: Option<bool> = sqlx::query_scalar(query)
        .bind(machine_id)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    if let Some(recorded) = recorded
        && (recorded || !requires_admin_ack)
    {
        return Ok(recorded);
    }

    let query = "UPDATE machines SET force_delete_requires_admin_ack = $2 WHERE id = $1
                 RETURNING force_delete_requires_admin_ack";
    sqlx::query_scalar(query)
        .bind(machine_id)
        .bind(requires_admin_ack)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

pub async fn get_network_config(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Versioned<ManagedHostNetworkConfig>, DatabaseError> {
    #[derive(FromRow)]
    struct QueryResult {
        network_config: sqlx::types::Json<ManagedHostNetworkConfig>,
        network_config_version: ConfigVersion,
    }

    let query = "SELECT network_config, network_config_version FROM machines WHERE id=$1";

    let QueryResult {
        network_config,
        network_config_version,
    } = sqlx::query_as(query)
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(Versioned::new(network_config.0, network_config_version))
}

pub async fn get_quarantine_state(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let network_config = get_network_config(txn, machine_id).await?;
    Ok(network_config.value.quarantine_state)
}

/// Sets quarantine and returns the previous state only after the conditional
/// write applies. Returns an error if the network configuration is missing or
/// changes before the write.
pub async fn set_quarantine_state(
    txn: &mut PgConnection,
    machine_id: &HostMachineId,
    quarantine_state: ManagedHostQuarantineState,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let (mut network_config, network_config_version) =
        get_network_config(&mut *txn, machine_id).await?.take();
    let old_quarantine_state = network_config.quarantine_state.clone();
    network_config.quarantine_state = Some(quarantine_state);
    match try_update_network_config(txn, machine_id, network_config_version, &network_config)
        .await?
    {
        ConditionalWrite::Applied(()) => Ok(old_quarantine_state),
        ConditionalWrite::NotApplied(MachineNetworkConfigNotCurrent) => {
            Err(DatabaseError::FailedPrecondition(format!(
                "network configuration for machine {machine_id} changed or is no longer available"
            )))
        }
    }
}

/// Clears quarantine and returns the previous state only after the conditional
/// write applies. Returns an error if the network configuration is missing or
/// changes before the write.
pub async fn clear_quarantine_state(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<Option<ManagedHostQuarantineState>, DatabaseError> {
    let (mut network_config, network_config_version) =
        get_network_config(&mut *txn, machine_id).await?.take();
    let old_quarantine_state = network_config.quarantine_state.clone();
    network_config.quarantine_state = None;
    match try_update_network_config(txn, machine_id, network_config_version, &network_config)
        .await?
    {
        ConditionalWrite::Applied(()) => Ok(old_quarantine_state),
        ConditionalWrite::NotApplied(MachineNetworkConfigNotCurrent) => {
            Err(DatabaseError::FailedPrecondition(format!(
                "network configuration for machine {machine_id} changed or is no longer available"
            )))
        }
    }
}

pub async fn modify_dpf_state(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    status: bool,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines set dpf = jsonb_set(dpf, '{enabled}', to_jsonb($1)) WHERE id=$2";
    sqlx::query(query)
        .bind(status)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn mark_machine_ingestion_done_with_dpf(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines set dpf = jsonb_set(dpf, '{used_for_ingestion}', to_jsonb(true)) WHERE id=$1";
    sqlx::query(query)
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn update_nvlink_info(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    nvlink_info: MachineNvLinkInfo,
) -> Result<(), DatabaseError> {
    let query = "UPDATE machines SET nvlink_info=$1 WHERE id=$2 RETURNING id";
    let _id: MachineId = sqlx::query_as(query)
        .bind(sqlx::types::Json(nvlink_info))
        .bind(machine_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct _HealthReportWrapper {
    hardware_health_report: Option<HealthReport>,
}
impl<'r> FromRow<'r, PgRow> for _HealthReportWrapper {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let hardware_health_report: sqlx::types::Json<Option<HealthReport>> =
            row.try_get("health_report")?;
        Ok(Self {
            hardware_health_report: hardware_health_report.0,
        })
    }
}

/// RMS identity for a compute tray machine, including rack profile context for
/// node descriptor construction.
#[derive(Debug, sqlx::FromRow)]
pub struct MachineRmsIdentity {
    pub id: String,
    pub bmc_ip: IpAddr,
    pub bmc_mac_address: MacAddress,
    pub rack_id: Option<RackId>,
    pub rack_profile_id: Option<RackProfileId>,
    /// Physical slot number last reported by RMS, or `None` if none has been recorded.
    pub slot_number: Option<i32>,
    /// Physical tray index last reported by RMS, or `None` if none has been recorded.
    pub tray_index: Option<i32>,
}

/// Look up RMS identities and rack profile context for compute tray machines by
/// their BMC IP addresses.
pub async fn find_rms_identities_by_bmc_ips(
    db: impl crate::db_read::DbReader<'_>,
    bmc_ips: &[IpAddr],
) -> DatabaseResult<Vec<MachineRmsIdentity>> {
    let ip_strings: Vec<String> = bmc_ips.iter().map(ToString::to_string).collect();
    let sql = r#"
        SELECT
            m.id::text,
            mia.address AS bmc_ip,
            mi.mac_address AS bmc_mac_address,
            m.rack_id,
            r.rack_profile_id,
            m.slot_number,
            m.tray_index
        FROM machines m
        LEFT JOIN racks r ON r.id = m.rack_id
        JOIN machine_interfaces mi
            ON mi.machine_id = m.id
            AND mi.interface_type = 'Bmc'
        JOIN machine_interface_addresses mia
            ON mia.interface_id = mi.id
        WHERE host(mia.address) = ANY($1)
    "#;

    let rows: Vec<MachineRmsIdentity> = sqlx::query_as(sql)
        .bind(ip_strings)
        .fetch_all(db)
        .await
        .map_err(|err| DatabaseError::new("machine::find_rms_identities_by_bmc_ips", err))?;

    let mut seen = std::collections::HashSet::with_capacity(rows.len());
    for row in &rows {
        if !seen.insert(row.bmc_ip) {
            return Err(DatabaseError::internal(format!(
                "duplicate machine RMS identity mapping for bmc_ip={}",
                row.bmc_ip
            )));
        }
    }
    Ok(rows)
}

pub fn count_healthy_unhealthy_host_machines(
    all_machines: &HashMap<HostMachineId, model::machine::ManagedHostStateSnapshot>,
) -> (i32, i32) {
    let without_fault_count = all_machines
        .iter()
        .filter(|(_, x)| {
            !x.aggregate_health
                .alerts
                .iter()
                .any(|x| x.id != *model::machine_update_module::HOST_UPDATE_HEALTH_PROBE_ID)
        })
        .count();

    (
        all_machines.len() as i32,
        (all_machines.len() - without_fault_count) as i32,
    )
}

#[cfg(test)]
mod test {
    use std::collections::{HashMap, HashSet};
    use std::net::{IpAddr, Ipv6Addr};
    use std::str::FromStr;
    use std::sync::Arc;

    use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
    use carbide_uuid::machine::{
        DpuMachineId, HostMachineId, HostMachineIdSubtypeTrait, MachineId, MachineIdSource,
        MachineInterfaceId, MachineType, StableHostMachineId,
    };
    use carbide_uuid::network::NetworkSegmentId;
    use model::allocation_type::AllocationType;
    use model::bmc_info::BmcInfo;
    use model::hardware_info::HardwareInfo;
    use model::machine::machine_search_config::MachineSearchConfig;
    use model::machine::topology::{DiscoveryData, TopologyData};
    use model::machine::{MachineState, ManagedHostState};
    use model::machine_interface::InterfaceType;
    use model::resource_pool::common::{
        CommonPools, DPA_VNI, EXTERNAL_VPC_VNI, EthernetPools, FNN_ASN, IbPools, LOOPBACK_IP,
        LOOPBACK_IP_V6, VLANID, VNI, VPC_DPU_LOOPBACK, VPC_VNI,
    };
    use model::resource_pool::define::{Range, ResourcePoolDef, ResourcePoolType};
    use model::resource_pool::{ResourcePool, ValueType};

    fn machine_id(seed: u8, ty: MachineType) -> MachineId {
        MachineId::new(MachineIdSource::ProductBoardChassisSerial, [seed; 32], ty)
    }

    #[crate::sqlx_test]
    async fn find_machine_ids_by_mac_addresses_preserves_sources(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use mac_address::MacAddress;
        use model::network_segment::NetworkSegmentType;
        use model::predicted_machine_interface::NewPredictedMachineInterface;

        let first_host = machine_id(40, MachineType::Host);
        let second_host = machine_id(41, MachineType::Host);
        let dpu = machine_id(42, MachineType::Dpu);
        let [
            live,
            ambiguous,
            bmc,
            unowned,
            dpu_mac,
            predicted,
            unrequested,
            unknown,
        ] = [1, 2, 3, 4, 5, 6, 7, 8].map(|suffix| MacAddress::new([0x02, 0, 0, 0, 0x32, suffix]));
        let mut txn = pool.begin().await?;
        for id in [first_host, second_host, dpu] {
            super::create(
                txn.as_mut(),
                None,
                &id,
                ManagedHostState::Ready,
                None,
                model::machine::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let segments: Vec<NetworkSegmentId> = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) \
             VALUES ('mac-owners-a', 'V1-T0'), ('mac-owners-b', 'V1-T0') RETURNING id",
        )
        .fetch_all(txn.as_mut())
        .await?;
        for (owner, segment, mac, interface_type) in [
            (Some(first_host), segments[0], live, InterfaceType::Data),
            (Some(first_host), segments[1], live, InterfaceType::Data),
            (
                Some(first_host),
                segments[0],
                ambiguous,
                InterfaceType::Data,
            ),
            (
                Some(second_host),
                segments[1],
                ambiguous,
                InterfaceType::Data,
            ),
            (Some(second_host), segments[0], bmc, InterfaceType::Bmc),
            (None, segments[0], unowned, InterfaceType::Data),
            (Some(dpu), segments[0], dpu_mac, InterfaceType::Data),
            (
                Some(first_host),
                segments[0],
                unrequested,
                InterfaceType::Data,
            ),
        ] {
            sqlx::query(
                "INSERT INTO machine_interfaces \
                    (machine_id, association_type, segment_id, mac_address, \
                     primary_interface, hostname, interface_type) \
                 VALUES ($1, $2::association_type, $3, $4, false, 'mac-owners', $5)",
            )
            .bind(owner)
            .bind(if owner.is_some() { "Machine" } else { "None" })
            .bind(segment)
            .bind(mac)
            .bind(interface_type)
            .execute(txn.as_mut())
            .await?;
        }
        for (mac_address, owner) in [
            (live, second_host),
            (bmc, first_host),
            (unowned, second_host),
            (predicted, first_host),
            (unrequested, second_host),
        ] {
            crate::predicted_machine_interface::create(
                NewPredictedMachineInterface {
                    machine_id: &owner,
                    mac_address,
                    expected_network_segment_type: NetworkSegmentType::HostInband,
                    boot_interface_id: None,
                    primary_interface: false,
                },
                txn.as_mut(),
            )
            .await?;
        }

        for (scenario, requested, expected_live, expected_predicted) in [
            ("empty request", vec![], vec![], vec![]),
            ("missing MAC", vec![unknown], vec![], vec![]),
            (
                "duplicate requested MAC with independent owners",
                vec![live, live],
                vec![(live, first_host)],
                vec![(live, second_host)],
            ),
            (
                "mixed sources exclude BMC and unowned real interfaces",
                vec![live, bmc, unowned, dpu_mac, predicted, unknown],
                vec![(live, first_host), (dpu_mac, dpu)],
                vec![
                    (live, second_host),
                    (bmc, first_host),
                    (unowned, second_host),
                    (predicted, first_host),
                ],
            ),
        ] {
            assert_eq!(
                super::find_machine_ids_by_mac_addresses(txn.as_mut(), &requested).await?,
                expected_live.into_iter().collect(),
                "{scenario}: real interfaces"
            );
            assert_eq!(
                crate::predicted_machine_interface::find_machine_ids_by_mac_addresses(
                    txn.as_mut(),
                    &requested,
                )
                .await?,
                expected_predicted.into_iter().collect(),
                "{scenario}: predictions"
            );
        }
        let owners = super::find_machine_ids_by_mac_addresses(txn.as_mut(), &[ambiguous]).await?;
        assert_eq!(owners.len(), 1);
        assert!([first_host, second_host].contains(&owners[&ambiguous]));
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn dpu_host_associations_load_and_validate(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use super::{DpuHostAssociationError, DpuHostAssociations};

        let [first_host, second_host] = [1, 2]
            .map(|seed| HostMachineId::try_from(machine_id(seed, MachineType::Host)).unwrap());
        let [first_dpu, second_dpu, third_dpu, unattached_dpu] = [3, 4, 5, 6]
            .map(|seed| DpuMachineId::try_from(machine_id(seed, MachineType::Dpu)).unwrap());
        let mut txn = pool.begin().await?;
        for id in [
            first_host.into(),
            second_host.into(),
            first_dpu.into(),
            second_dpu.into(),
            third_dpu.into(),
            unattached_dpu.into(),
        ] {
            super::create(
                txn.as_mut(),
                None,
                &id,
                ManagedHostState::Ready,
                None,
                model::machine::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('dpu-host-associations', 'V1-T0') RETURNING id",
        )
        .fetch_one(txn.as_mut())
        .await?;
        for (index, (dpu_id, owner_id, interface_type)) in [
            (first_dpu, MachineId::from(first_host), InterfaceType::Data),
            (second_dpu, first_host.into(), InterfaceType::Data),
            (third_dpu, second_host.into(), InterfaceType::Data),
            // Multiple interface rows still represent one attachment for this DPU.
            (first_dpu, first_host.into(), InterfaceType::Data),
            // Neither a DPU's own OOB row nor a BMC row is a host attachment.
            (unattached_dpu, unattached_dpu.into(), InterfaceType::Data),
            (unattached_dpu, second_host.into(), InterfaceType::Bmc),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, segment_id,
                     mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, false, 'dpu-host-associations', $5)",
            )
            .bind(owner_id)
            .bind(dpu_id)
            .bind(segment_id)
            .bind(format!("02:00:00:00:31:{index:02x}"))
            .bind(interface_type)
            .execute(txn.as_mut())
            .await?;
        }

        for (scenario, requested, expected) in [
            ("empty request", vec![], Ok(None)),
            ("no host attachments", vec![unattached_dpu], Ok(None)),
            ("one DPU", vec![first_dpu], Ok(Some(first_host))),
            (
                "complete shared host with duplicate requested IDs",
                vec![second_dpu, first_dpu, first_dpu],
                Ok(Some(first_host)),
            ),
            (
                "partial attachment",
                vec![first_dpu, unattached_dpu],
                Err(DpuHostAssociationError::Incomplete {
                    dpu_machine_id: unattached_dpu,
                }),
            ),
            (
                "different hosts",
                vec![first_dpu, third_dpu],
                Err(DpuHostAssociationError::MultipleHosts),
            ),
            (
                "partial attachment takes precedence over different hosts",
                vec![first_dpu, third_dpu, unattached_dpu],
                Err(DpuHostAssociationError::Incomplete {
                    dpu_machine_id: unattached_dpu,
                }),
            ),
        ] {
            let associations = DpuHostAssociations::load(txn.as_mut(), &requested).await?;
            assert_eq!(
                associations.common_host_machine_id(),
                expected,
                "{scenario}"
            );
        }

        let associations =
            DpuHostAssociations::load(txn.as_mut(), &[first_dpu, second_dpu, third_dpu]).await?;
        assert_eq!(associations.host_machine_id(&first_dpu), Some(first_host));
        assert_eq!(associations.host_machine_id(&third_dpu), Some(second_host));
        assert_eq!(associations.host_machine_id(&unattached_dpu), None);
        let mut host_ids = associations.host_machine_ids().collect::<Vec<_>>();
        host_ids.sort_unstable();
        let mut expected_host_ids = vec![first_host, first_host, second_host];
        expected_host_ids.sort_unstable();
        assert_eq!(host_ids, expected_host_ids);

        txn.rollback().await?;
        Ok(())
    }

    // Raw stored JSON and independent expectations verify which health report is
    // selected and which corrupt inputs are rejected.
    #[crate::sqlx_test]
    async fn dpu_network_status_inputs_preserve_typed_values_and_nulls(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use config_version::{ConfigVersion, Versioned};
        use health_report::{HealthProbeAlert, HealthReport};
        use model::instance::status::network::InstanceNetworkStatusObservation;
        use model::machine::network::{MachineNetworkStatusObservation, ManagedHostNetworkConfig};
        use serde_json::json;

        use super::DpuNetworkStatusInputs;

        struct Case<'a> {
            scenario: &'static str,
            network_config: serde_json::Value,
            network_config_version: &'static str,
            observation: Option<serde_json::Value>,
            health_reports: Option<serde_json::Value>,
            expect: Result<&'a DpuNetworkStatusInputs, &'static str>,
        }

        let dpu_id = DpuMachineId::try_from(machine_id(1, MachineType::Dpu))?;
        let config_version_string = "V7-T1722000000000000";
        let config_version: ConfigVersion = config_version_string.parse()?;
        let observed_at = "2026-10-24T16:00:00Z".parse()?;
        let populated = DpuNetworkStatusInputs {
            network_config: Versioned::new(
                ManagedHostNetworkConfig {
                    loopback_ip: None,
                    loopback_ip_v6: None,
                    use_admin_network: Some(false),
                    quarantine_state: None,
                    use_admin_network_changed: Some(true),
                },
                config_version,
            ),
            network_status_observation: Some(MachineNetworkStatusObservation {
                machine_id: dpu_id,
                agent_version: Some("status-inputs-test".to_string()),
                observed_at,
                network_config_version: Some("V6-T1722000000000000".parse()?),
                client_certificate_expiry: None,
                agent_version_superseded_at: None,
                instance_network_observation: Some(InstanceNetworkStatusObservation {
                    config_version: "V4-T1722000000000000".parse()?,
                    instance_config_version: Some("V3-T1722000000000000".parse()?),
                    interfaces: Vec::new(),
                    observed_at,
                }),
                fabric_interfaces: Vec::new(),
                accepted_lldp_snapshot_fingerprint: None,
            }),
            dpu_agent_health_report: Some(HealthReport {
                source: HealthReport::DPU_AGENT_SOURCE.to_string(),
                triggered_by: None,
                observed_at: Some(observed_at),
                successes: Vec::new(),
                alerts: vec![HealthProbeAlert {
                    id: "NetworkConfig".parse()?,
                    target: None,
                    in_alert_since: Some("2026-10-24T15:00:00Z".parse()?),
                    message: "network configuration failed".to_string(),
                    tenant_message: None,
                    classifications: Vec::new(),
                }],
            }),
        };
        let absent = DpuNetworkStatusInputs {
            network_config: Versioned::new(
                ManagedHostNetworkConfig {
                    loopback_ip: None,
                    loopback_ip_v6: None,
                    use_admin_network: None,
                    quarantine_state: None,
                    use_admin_network_changed: None,
                },
                config_version,
            ),
            network_status_observation: None,
            dpu_agent_health_report: None,
        };
        let observation = json!({
            "machine_id": dpu_id,
            "agent_version": "status-inputs-test",
            "observed_at": "2026-10-24T16:00:00Z",
            "network_config_version": "V6-T1722000000000000",
            "instance_network_observation": {
                "config_version": "V4-T1722000000000000",
                "instance_config_version": "V3-T1722000000000000",
                "observed_at": "2026-10-24T16:00:00Z"
            }
        });
        let dpu_health = json!({
            "source": "forge-dpu-agent",
            "observed_at": "2026-10-24T16:00:00Z",
            "successes": [],
            "alerts": [{
                "id": "NetworkConfig",
                "in_alert_since": "2026-10-24T15:00:00Z",
                "message": "network configuration failed",
                "classifications": []
            }]
        });
        let mut replacement = dpu_health.clone();
        replacement["alerts"][0]["in_alert_since"] = json!("2026-10-24T14:00:00Z");
        let mut other_source = dpu_health.clone();
        other_source["source"] = json!("a-other-source");
        other_source["alerts"][0]["in_alert_since"] = json!("2026-10-24T13:00:00Z");
        let mut malformed_observation = observation.clone();
        malformed_observation["network_config_version"] = json!("invalid-version");

        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &dpu_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        for case in [
            Case {
                scenario: "typed inputs use only DPU merge health and selected machine fields",
                network_config: json!({
                    "use_admin_network": false,
                    "use_admin_network_changed": true
                }),
                network_config_version: config_version_string,
                observation: Some(observation),
                health_reports: Some(json!({
                    "replace": replacement,
                    "merges": {"forge-dpu-agent": dpu_health, "a-other-source": other_source}
                })),
                expect: Ok(&populated),
            },
            Case {
                scenario: "SQL-null observation and JSON-null health with omitted pending flag",
                network_config: json!({}),
                network_config_version: config_version_string,
                observation: None,
                health_reports: Some(json!(null)),
                expect: Ok(&absent),
            },
            Case {
                scenario: "JSON-null observation and SQL-null health with null pending flag",
                network_config: json!({"use_admin_network_changed": null}),
                network_config_version: config_version_string,
                observation: Some(json!(null)),
                health_reports: None,
                expect: Ok(&absent),
            },
            Case {
                scenario: "a pending flag string is not a JSON boolean",
                network_config: json!({"use_admin_network_changed": "true"}),
                network_config_version: config_version_string,
                observation: None,
                health_reports: None,
                expect: Err("network_config"),
            },
            Case {
                scenario: "a malformed desired config version is a decoding error",
                network_config: json!({}),
                network_config_version: "invalid-version",
                observation: None,
                health_reports: None,
                expect: Err("network_config_version"),
            },
            Case {
                scenario: "a malformed observation is not absent",
                network_config: json!({}),
                network_config_version: config_version_string,
                observation: Some(malformed_observation),
                health_reports: None,
                expect: Err("network_status_observation"),
            },
            Case {
                scenario: "a null merge report is not absent",
                network_config: json!({}),
                network_config_version: config_version_string,
                observation: None,
                health_reports: Some(json!({"merges": {"forge-dpu-agent": null}})),
                expect: Err("health_reports"),
            },
            Case {
                scenario: "a malformed unrelated merge report is still a decoding error",
                network_config: json!({}),
                network_config_version: config_version_string,
                observation: None,
                health_reports: Some(json!({
                    "merges": {"forge-dpu-agent": dpu_health, "a-other-source": {}}
                })),
                expect: Err("health_reports"),
            },
            Case {
                scenario: "replacement and other merge reports do not supply missing DPU health",
                network_config: json!({}),
                network_config_version: config_version_string,
                observation: None,
                health_reports: Some(json!({
                    "replace": replacement,
                    "merges": {"a-other-source": other_source}
                })),
                expect: Ok(&absent),
            },
        ] {
            // The status read must not decode unrelated controller state.
            sqlx::query(
                "UPDATE machines SET network_config = $2, network_config_version = $3,
                     network_status_observation = $4, health_reports = $5,
                     controller_state = '{}'::jsonb WHERE id = $1",
            )
            .bind(dpu_id)
            .bind(case.network_config)
            .bind(case.network_config_version)
            .bind(case.observation)
            .bind(case.health_reports)
            .execute(txn.as_mut())
            .await?;

            let result = super::find_dpu_network_status_inputs(txn.as_mut(), &dpu_id).await;
            match case.expect {
                Ok(expected) => {
                    let actual = result.expect(case.scenario).expect(case.scenario);
                    assert_eq!(
                        actual.network_config.value, expected.network_config.value,
                        "{}",
                        case.scenario
                    );
                    assert_eq!(
                        actual.network_config.version, expected.network_config.version,
                        "{}",
                        case.scenario
                    );
                    assert_eq!(
                        actual.network_status_observation, expected.network_status_observation,
                        "{}",
                        case.scenario
                    );
                    assert_eq!(
                        actual.dpu_agent_health_report, expected.dpu_agent_health_report,
                        "{}",
                        case.scenario
                    );
                }
                Err(column) => {
                    let error = result.expect_err(case.scenario);
                    let crate::DatabaseError::Sqlx(annotated) = &error else {
                        panic!("{}: expected Sqlx, got {error:?}", case.scenario);
                    };
                    let sqlx::Error::ColumnDecode { index, .. } = &annotated.source else {
                        panic!("{}: expected ColumnDecode, got {error:?}", case.scenario);
                    };
                    assert_eq!(index, &format!("{column:?}"), "{}", case.scenario);
                }
            }
        }
        txn.rollback().await?;
        Ok(())
    }

    // One host interface and its DPU make the membership and row-lock boundaries
    // visible without depending on the rest of a managed-host fixture.
    async fn nic_firmware_failure_fixture(
        pool: &sqlx::PgPool,
        seed: u8,
    ) -> Result<(HostMachineId, DpuMachineId, MachineInterfaceId), Box<dyn std::error::Error>> {
        let host_id = HostMachineId::try_from(machine_id(seed, MachineType::Host))?;
        let dpu_id = DpuMachineId::try_from(machine_id(seed, MachineType::Dpu))?;
        let state = ManagedHostState::NicFirmwareUpdating {
            operation: model::test_support::nic_firmware::operation(),
        };
        let mut txn = pool.begin().await?;
        for id in [MachineId::from(host_id), MachineId::from(dpu_id)] {
            super::create(txn.as_mut(), None, &id, state.clone(), None, 2).await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind(format!("nic-failure-{seed}"))
        .fetch_one(txn.as_mut())
        .await?;
        let interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (machine_id, attached_dpu_machine_id, association_type, segment_id,
                  mac_address, primary_interface, hostname, interface_type)
             VALUES ($1, $2, 'Machine', $3, $4::macaddr,
                     false, $5, 'Data') RETURNING id",
        )
        .bind(host_id)
        .bind(dpu_id)
        .bind(segment_id)
        .bind(format!("02:00:00:00:00:{seed:02x}"))
        .bind(format!("nic-failure-{seed}"))
        .fetch_one(txn.as_mut())
        .await?;
        txn.commit().await?;
        Ok((host_id, dpu_id, interface_id))
    }

    // Cleanup callbacks can record failure without changing the controller
    // version. Completion must reread both the host and its attached DPUs.
    #[crate::sqlx_test]
    async fn nic_firmware_completion_rejects_failures_recorded_after_its_snapshot(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::{FailureCause, FailureDetails, FailureSource};
        use model::test_support::nic_firmware::operation;

        for (seed, scenario, fail_host) in [(60, "host failure", true), (61, "DPU failure", false)]
        {
            let (host_id, dpu_id, _) = nic_firmware_failure_fixture(&pool, seed).await?;
            let mut snapshot_txn = pool.begin().await?;
            let original = super::find_one(
                snapshot_txn.as_mut(),
                &host_id,
                MachineSearchConfig {
                    include_history: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("firmware host before cleanup failure");
            assert!(
                !super::has_recorded_managed_host_failure(snapshot_txn.as_mut(), &host_id).await?
            );
            snapshot_txn.commit().await?;

            let failed_machine = if fail_host {
                host_id.into()
            } else {
                dpu_id.into()
            };
            let failure = FailureDetails {
                cause: FailureCause::NVMECleanFailed {
                    err: scenario.into(),
                },
                source: FailureSource::Scout,
                failed_at: chrono::Utc::now(),
            };
            let mut writer = pool.begin().await?;
            super::update_failure_details_by_machine_id(
                &failed_machine,
                writer.as_mut(),
                failure.clone(),
            )
            .await?;
            writer.commit().await?;

            let mut completion = pool.begin().await?;
            assert_eq!(
                super::prepare_nic_firmware_completion(
                    completion.as_mut(),
                    &host_id,
                    original.state.version,
                    &operation().purpose,
                )
                .await?,
                crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent),
                "{scenario}",
            );
            completion.commit().await?;
            let saved = super::find_one(
                &pool,
                &host_id,
                MachineSearchConfig {
                    include_history: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("firmware host after rejected completion");
            assert_eq!(saved.state.value, original.state.value, "{scenario}");
            assert_eq!(saved.state.version, original.state.version, "{scenario}");
            assert_eq!(
                serde_json::to_value(saved.history)?,
                serde_json::to_value(original.history)?,
                "{scenario}"
            );
            let failed = super::find_one(&pool, &failed_machine, MachineSearchConfig::default())
                .await?
                .expect("recorded cleanup failure");
            assert_eq!(failed.status.failure_details, failure, "{scenario}");
        }
        Ok(())
    }

    // A BMC association does not make a DPU part of the host's failure set.
    // The same recorded failure becomes relevant when that link becomes `Data`.
    #[crate::sqlx_test]
    async fn nic_firmware_failure_reader_uses_only_non_bmc_attachments(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::{FailureCause, FailureDetails, FailureSource};

        let (host_id, dpu_id, interface_id) = nic_firmware_failure_fixture(&pool, 66).await?;
        let mut txn = pool.begin().await?;
        sqlx::query("UPDATE machine_interfaces SET interface_type = 'Bmc' WHERE id = $1")
            .bind(interface_id)
            .execute(txn.as_mut())
            .await?;
        super::update_failure_details_by_machine_id(
            &dpu_id.into(),
            txn.as_mut(),
            FailureDetails {
                cause: FailureCause::NVMECleanFailed {
                    err: "BMC-only DPU failure".into(),
                },
                source: FailureSource::Scout,
                failed_at: chrono::Utc::now(),
            },
        )
        .await?;
        assert!(!super::has_recorded_managed_host_failure(txn.as_mut(), &host_id).await?);

        sqlx::query("UPDATE machine_interfaces SET interface_type = 'Data' WHERE id = $1")
            .bind(interface_id)
            .execute(txn.as_mut())
            .await?;
        assert!(super::has_recorded_managed_host_failure(txn.as_mut(), &host_id).await?);
        txn.rollback().await?;
        Ok(())
    }

    // The host fence must defer instead of waiting behind DPU-first writers.
    // Rejected preparation releases its locks so that writer can finish.
    #[crate::sqlx_test]
    async fn nic_firmware_completion_defers_busy_dpu_and_membership_locks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::{FailureCause, FailureDetails, FailureSource};
        use model::test_support::nic_firmware::operation;

        enum Boundary {
            DpuHistory,
            DpuRow,
            InterfaceRebind,
        }
        for (seed, scenario, boundary) in [
            (62, "DPU history is busy", Boundary::DpuHistory),
            (63, "DPU row is busy", Boundary::DpuRow),
            (
                64,
                "BMC interface is becoming a data attachment",
                Boundary::InterfaceRebind,
            ),
        ] {
            let (host_id, dpu_id, interface_id) = nic_firmware_failure_fixture(&pool, seed).await?;
            if matches!(boundary, Boundary::InterfaceRebind) {
                sqlx::query("UPDATE machine_interfaces SET interface_type = 'Bmc' WHERE id = $1")
                    .bind(interface_id)
                    .execute(&pool)
                    .await?;
            }
            let original = super::find_one(
                &pool,
                &host_id,
                MachineSearchConfig {
                    include_history: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("firmware host before contended completion");
            let mut writer = pool.begin().await?;
            match boundary {
                Boundary::DpuHistory => {
                    sqlx::query(
                        "SELECT pg_advisory_xact_lock(\
                        hashtextextended($1, 'machine_state_history'::regclass::bigint)\
                    )",
                    )
                    .bind(dpu_id.to_string())
                    .execute(writer.as_mut())
                    .await?;
                }
                Boundary::DpuRow => {
                    super::lock_by_id_for_update(writer.as_mut(), &dpu_id.into()).await?;
                }
                Boundary::InterfaceRebind => {
                    sqlx::query(
                        "UPDATE machine_interfaces SET interface_type = 'Data' WHERE id = $1",
                    )
                    .bind(interface_id)
                    .execute(writer.as_mut())
                    .await?;
                }
            }
            let mut completion = pool.begin().await?;
            let deferred = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                super::prepare_nic_firmware_completion(
                    completion.as_mut(),
                    &host_id,
                    original.state.version,
                    &operation().purpose,
                ),
            )
            .await??;
            assert_eq!(
                deferred,
                crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent),
                "{scenario}"
            );

            // Keep the outer completion transaction open: only rollback of the
            // helper's savepoint makes the host available to this writer.
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(host_id)
                .fetch_one(writer.as_mut())
                .await?;
            writer.commit().await?;
            assert_eq!(
                super::prepare_nic_firmware_completion(
                    completion.as_mut(),
                    &host_id,
                    original.state.version,
                    &operation().purpose,
                )
                .await?,
                crate::ConditionalWrite::Applied(()),
                "{scenario}: clearing contention must permit retry",
            );

            if matches!(boundary, Boundary::DpuRow) {
                // One healthy retry also proves lock strength and retention.
                // `FOR KEY SHARE` would let this failure UPDATE pass early.
                let original_dpu = super::find_one(
                    completion.as_mut(),
                    &dpu_id,
                    MachineSearchConfig {
                        include_history: true,
                        ..Default::default()
                    },
                )
                .await?
                .expect("DPU under successful completion fence");
                let completion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(completion.as_mut())
                    .await?;
                let new_version = original.state.version.increment();
                let failure = FailureDetails {
                    cause: FailureCause::NVMECleanFailed {
                        err: "failure writer waiting for completion".into(),
                    },
                    source: FailureSource::Scout,
                    failed_at: chrono::Utc::now(),
                };
                let failure_write = async {
                    let mut writer = pool.begin().await?;
                    super::update_failure_details_by_machine_id(
                        &dpu_id.into(),
                        writer.as_mut(),
                        failure.clone(),
                    )
                    .await?;
                    writer.commit().await?;
                    Ok::<_, Box<dyn std::error::Error>>(())
                };
                let commit_completion = async {
                    crate::test_support::postgres::wait_for_blocked_query(
                        &pool,
                        completion_pid,
                        "UPDATE machines SET failure_details",
                    )
                    .await;
                    assert_eq!(
                        super::try_update_controller_state(
                            completion.as_mut(),
                            &host_id,
                            original.state.version,
                            new_version,
                            &ManagedHostState::Ready,
                        )
                        .await?,
                        crate::ConditionalWrite::Applied(()),
                    );
                    assert_eq!(
                        crate::test_support::postgres::count_blocked_queries(
                            &pool,
                            completion_pid,
                            "UPDATE machines SET failure_details",
                        )
                        .await,
                        1,
                        "the failure writer must remain blocked after CAS and before commit",
                    );
                    completion.commit().await?;
                    Ok::<_, Box<dyn std::error::Error>>(())
                };
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::try_join!(failure_write, commit_completion)
                })
                .await??;

                let saved = super::find_one(
                    &pool,
                    &host_id,
                    MachineSearchConfig {
                        include_history: true,
                        ..Default::default()
                    },
                )
                .await?
                .expect("host after successful completion");
                assert_eq!(saved.state.value, ManagedHostState::Ready);
                assert_eq!(saved.state.version, new_version);
                assert_eq!(saved.history.len(), original.history.len() + 1);
                let transition = saved
                    .history
                    .iter()
                    .find(|entry| entry.state_version == new_version)
                    .expect("completion history entry");
                assert_eq!(transition.state_version, new_version);
                assert_eq!(
                    serde_json::from_str::<ManagedHostState>(&transition.state)?,
                    ManagedHostState::Ready
                );
                let dpu = super::find_one(
                    &pool,
                    &dpu_id,
                    MachineSearchConfig {
                        include_history: true,
                        ..Default::default()
                    },
                )
                .await?
                .expect("DPU after completion and released failure writer");
                assert_eq!(dpu.state.value, ManagedHostState::Ready);
                assert_eq!(dpu.state.version, new_version);
                assert_eq!(dpu.history.len(), original_dpu.history.len() + 1);
                assert_eq!(dpu.status.failure_details, failure);
            } else {
                // The remaining boundaries only need to prove retry eligibility;
                // the DPU-row case above owns the successful CAS/history proof.
                completion.rollback().await?;
                let saved = super::find_one(
                    &pool,
                    &host_id,
                    MachineSearchConfig {
                        include_history: true,
                        ..Default::default()
                    },
                )
                .await?
                .expect("host after successful preparation probe");
                assert_eq!(saved.state.value, original.state.value, "{scenario}");
                assert_eq!(saved.state.version, original.state.version, "{scenario}");
                assert_eq!(
                    serde_json::to_value(saved.history)?,
                    serde_json::to_value(original.history)?,
                    "{scenario}"
                );
            }
        }
        Ok(())
    }

    // Preparation leaves state and history to the controller CAS. A contender
    // that waited for that transaction must reject its stale snapshot.
    #[crate::sqlx_test]
    async fn nic_firmware_completion_rejects_a_stale_contender_after_commit(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::test_support::nic_firmware::operation;

        let host_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let operation = operation();
        let initial_state = ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        };
        let mut setup_txn = pool.begin().await?;
        super::create(
            setup_txn.as_mut(),
            None,
            &host_id.into(),
            initial_state.clone(),
            None,
            2,
        )
        .await?;
        setup_txn.commit().await?;
        let machine = super::find_one(
            &pool,
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("fixture host");
        let expected_version = machine.state.version;
        let winner_version = expected_version.increment();
        let mut winner = pool.begin().await?;
        let winner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(winner.as_mut())
            .await?;
        assert_eq!(
            super::prepare_nic_firmware_completion(
                winner.as_mut(),
                &host_id,
                expected_version,
                &operation.purpose,
            )
            .await?,
            crate::ConditionalWrite::Applied(())
        );

        // Only the controller CAS may change state or append history.
        let prepared = super::find_one(
            winner.as_mut(),
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("prepared host");
        assert_eq!(prepared.state.value, machine.state.value);
        assert_eq!(prepared.state.version, machine.state.version);
        assert_eq!(
            serde_json::to_value(&prepared.history)?,
            serde_json::to_value(&machine.history)?,
        );
        assert_eq!(
            super::try_update_controller_state(
                winner.as_mut(),
                &host_id,
                expected_version,
                winner_version,
                &ManagedHostState::Ready,
            )
            .await?,
            crate::ConditionalWrite::Applied(())
        );

        // Another reader still sees the persisted state until commit.
        let before_commit = super::find_one(&pool, &host_id, MachineSearchConfig::default())
            .await?
            .expect("uncommitted host");
        assert_eq!(before_commit.state.value, initial_state);
        let losing_write = async {
            let mut loser = pool.begin().await?;
            let result = super::prepare_nic_firmware_completion(
                loser.as_mut(),
                &host_id,
                expected_version,
                &operation.purpose,
            )
            .await?;
            if result == crate::ConditionalWrite::Applied(()) {
                assert_eq!(
                    super::try_update_controller_state(
                        loser.as_mut(),
                        &host_id,
                        expected_version,
                        expected_version.increment(),
                        &ManagedHostState::ForceDeletion,
                    )
                    .await?,
                    crate::ConditionalWrite::Applied(())
                );
            }
            loser.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        let commit_winner = async {
            crate::test_support::postgres::wait_for_blocked_query(
                &pool,
                winner_pid,
                "SELECT pg_advisory_xact_lock",
            )
            .await;
            winner.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(losing_write, commit_winner)
        })
        .await??;
        assert_eq!(
            result,
            crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent)
        );
        let saved = super::find_one(
            &pool,
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("completed host");
        assert_eq!(saved.state.value, ManagedHostState::Ready);
        assert_eq!(saved.state.version, winner_version);
        assert_eq!(saved.history.len(), machine.history.len() + 1);

        Ok(())
    }

    // Editing an Instance does not replace its allocation. Completion must
    // wait for the writer, then accept the same live attached owner.
    #[crate::sqlx_test]
    async fn nic_firmware_completion_keeps_the_owner_after_a_concurrent_config_edit(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::instance::InstanceId;
        use config_version::ConfigVersion;
        use model::machine::InstanceState;
        use model::machine::nic_firmware::NicFirmwarePurpose;
        use model::test_support::nic_firmware::operation;

        let host_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let instance_id = InstanceId::new();
        let config_version = ConfigVersion::initial();
        let edited_config_version = config_version.increment();
        let mut operation = operation();
        operation.purpose = NicFirmwarePurpose::Allocation { instance_id };
        let initial_state = ManagedHostState::NicFirmwareUpdating {
            operation: operation.clone(),
        };
        let mut setup = pool.begin().await?;
        super::create(
            setup.as_mut(),
            None,
            &host_id.into(),
            initial_state.clone(),
            None,
            2,
        )
        .await?;
        sqlx::query("INSERT INTO instances (id, machine_id, config_version, network_config) VALUES ($1, $2, $3, '{\"interfaces\": []}')")
            .bind(instance_id).bind(host_id).bind(config_version).execute(setup.as_mut()).await?;
        setup.commit().await?;
        let machine = super::find_one(&pool, &host_id, MachineSearchConfig::default())
            .await?
            .expect("allocated host");
        let completed_state_version = machine.state.version.increment();
        let mut writer = pool.begin().await?;
        let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await?;
        sqlx::query("UPDATE instances SET config_version = $1 WHERE id = $2")
            .bind(edited_config_version)
            .bind(instance_id)
            .execute(writer.as_mut())
            .await?;
        let continuation = ManagedHostState::Assigned {
            instance_state: InstanceState::DpaProvisioning,
        };
        let complete = async {
            let mut txn = pool.begin().await?;
            let result = super::prepare_nic_firmware_completion(
                txn.as_mut(),
                &host_id,
                machine.state.version,
                &operation.purpose,
            )
            .await?;
            if result == crate::ConditionalWrite::Applied(()) {
                assert_eq!(
                    super::try_update_controller_state(
                        txn.as_mut(),
                        &host_id,
                        machine.state.version,
                        completed_state_version,
                        &continuation,
                    )
                    .await?,
                    crate::ConditionalWrite::Applied(())
                );
            }
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        let commit_edit = async {
            crate::test_support::postgres::wait_for_blocked_query(
                &pool,
                writer_pid,
                "SELECT machine_id, deleted",
            )
            .await;
            writer.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(complete, commit_edit)
        })
        .await??;
        assert_eq!(result, crate::ConditionalWrite::Applied(()));
        let saved = super::find_one(&pool, &host_id, MachineSearchConfig::default())
            .await?
            .expect("completed host");
        assert_eq!(saved.state.value, continuation);
        assert_eq!(saved.state.version, completed_state_version);
        let saved_config_version: ConfigVersion =
            sqlx::query_scalar("SELECT config_version FROM instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(saved_config_version, edited_config_version);
        Ok(())
    }

    // Deleted-owner release is a lifecycle handoff too: a DPU cleanup failure
    // recorded after its clean snapshot must still keep the firmware state.
    #[crate::sqlx_test]
    async fn nic_firmware_owner_release_rechecks_attached_dpu_failure(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::instance::InstanceId;
        use model::machine::nic_firmware::{NicFirmwarePhase, NicFirmwarePurpose};
        use model::machine::{FailureCause, FailureDetails, FailureSource};
        use model::test_support::nic_firmware::operation;

        let (host_id, dpu_id, _) = nic_firmware_failure_fixture(&pool, 65).await?;
        let instance_id = InstanceId::new();
        let mut operation = operation();
        operation.purpose = NicFirmwarePurpose::Allocation { instance_id };
        operation.phase = NicFirmwarePhase::Failed {
            reason: "lockdown safely restored".into(),
        };
        let mut setup = pool.begin().await?;
        sqlx::query(
            "INSERT INTO instances (id, machine_id, deleted, network_config)
                     VALUES ($1, $2, clock_timestamp(), '{\"interfaces\": []}')",
        )
        .bind(instance_id)
        .bind(host_id)
        .execute(setup.as_mut())
        .await?;
        super::update_state(
            setup.as_mut(),
            &host_id,
            &ManagedHostState::NicFirmwareUpdating { operation },
        )
        .await?;
        let original = super::find_one(
            setup.as_mut(),
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("safely restored firmware operation");
        assert!(!super::has_recorded_managed_host_failure(setup.as_mut(), &host_id).await?);
        setup.commit().await?;

        let failure = FailureDetails {
            cause: FailureCause::NVMECleanFailed {
                err: "late DPU cleanup failure".into(),
            },
            source: FailureSource::Scout,
            failed_at: chrono::Utc::now(),
        };
        let mut writer = pool.begin().await?;
        super::update_failure_details_by_machine_id(
            &dpu_id.into(),
            writer.as_mut(),
            failure.clone(),
        )
        .await?;
        writer.commit().await?;
        let mut release = pool.begin().await?;
        assert_eq!(
            super::prepare_nic_firmware_owner_release(
                release.as_mut(),
                &host_id,
                original.state.version,
                &instance_id,
            )
            .await?,
            crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent),
        );
        // Owner release retains its locks on rejection; unlike completion, it
        // lets its caller choose when to end the surrounding transaction.
        let mut probe = pool.begin().await?;
        assert!(!super::try_lock_by_id_for_update(probe.as_mut(), &host_id.into()).await?);
        probe.rollback().await?;
        release.commit().await?;
        let saved = super::find_one(
            &pool,
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("retained firmware operation");
        assert_eq!(saved.state.value, original.state.value);
        assert_eq!(saved.state.version, original.state.version);
        assert_eq!(
            serde_json::to_value(saved.history)?,
            serde_json::to_value(original.history)?
        );
        let dpu = super::find_one(&pool, &dpu_id, MachineSearchConfig::default())
            .await?
            .expect("failed DPU remains attached");
        assert_eq!(dpu.status.failure_details, failure);
        Ok(())
    }

    // Recovery can outlive its deleted Instance. Waiting on that row must
    // not authorize release of the replacement attached during the wait.
    #[crate::sqlx_test]
    async fn nic_firmware_owner_release_rejects_replacement_after_waiting(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::instance::InstanceId;

        let host_id = HostMachineId::try_from(machine_id(29, MachineType::Host))?;
        let instance_id = InstanceId::new();
        let replacement_id = InstanceId::new();
        let mut setup = pool.begin().await?;
        super::create(
            setup.as_mut(),
            None,
            &host_id.into(),
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        sqlx::query("INSERT INTO instances (id, machine_id, deleted, network_config) VALUES ($1, $2, clock_timestamp(), '{\"interfaces\": []}')")
            .bind(instance_id).bind(host_id).execute(setup.as_mut()).await?;
        setup.commit().await?;
        let original = super::find_one(
            &pool,
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("host with deleted allocation");

        // Pause removal before commit. The prerequisite must wait for the
        // original instance and then see that it no longer owns this host.
        let mut replacement = pool.begin().await?;
        let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(replacement.as_mut())
            .await?;
        sqlx::query("DELETE FROM instances WHERE id = $1")
            .bind(instance_id)
            .execute(replacement.as_mut())
            .await?;
        sqlx::query("INSERT INTO instances (id, machine_id, network_config) VALUES ($1, $2, '{\"interfaces\": []}')")
            .bind(replacement_id).bind(host_id).execute(replacement.as_mut()).await?;
        let prepare_release = async {
            let mut txn = pool.begin().await?;
            let prepared = super::prepare_nic_firmware_owner_release(
                txn.as_mut(),
                &host_id,
                original.state.version,
                &instance_id,
            )
            .await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(prepared)
        };
        let commit_replacement = async {
            crate::test_support::postgres::wait_for_blocked_query(
                &pool,
                writer_pid,
                "SELECT id FROM instances",
            )
            .await;
            replacement.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (prepared, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(prepare_release, commit_replacement)
        })
        .await??;
        assert_eq!(
            prepared,
            crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent)
        );
        let attached: InstanceId =
            sqlx::query_scalar("SELECT id FROM instances WHERE machine_id = $1")
                .bind(host_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(attached, replacement_id);
        let saved = super::find_one(
            &pool,
            &host_id,
            MachineSearchConfig {
                include_history: true,
                ..Default::default()
            },
        )
        .await?
        .expect("host with replacement allocation");
        assert_eq!(saved.state.value, original.state.value);
        assert_eq!(saved.state.version, original.state.version);
        assert_eq!(
            serde_json::to_value(saved.history)?,
            serde_json::to_value(original.history)?
        );
        Ok(())
    }

    // Completion checks the stored attachment as well as the named owner.
    // A removed cleanup owner is allowed only without a replacement.
    #[crate::sqlx_test]
    async fn nic_firmware_completion_checks_attached_instance(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::instance::InstanceId;
        use config_version::ConfigVersion;
        use model::machine::nic_firmware::{NicFirmwareBaselineContinuation, NicFirmwarePurpose};
        use model::machine::{CleanupContext, CleanupState, InstanceState};
        use model::test_support::nic_firmware::operation;

        struct Case {
            scenario: &'static str,
            purpose: NicFirmwarePurpose,
            attached_instance: Option<(InstanceId, bool)>,
            attachment_host: Option<HostMachineId>,
            applies: bool,
        }

        let foreign_host_id = HostMachineId::try_from(machine_id(90, MachineType::Host))?;
        let foreign_instance_id = InstanceId::new();
        let allocation_id = InstanceId::new();
        let restore_owner_id = InstanceId::new();
        let replacement_id = InstanceId::new();
        let config_version = ConfigVersion::initial();
        let restore = NicFirmwarePurpose::RestoreBaseline {
            instance_id: InstanceId::new(),
            cleanup_context: CleanupContext::Deprovision,
        };
        let cases = [
            // A valid named owner on another host cannot authorize this host.
            Case {
                scenario: "the named owner belongs to another host",
                purpose: NicFirmwarePurpose::Allocation {
                    instance_id: foreign_instance_id,
                },
                attached_instance: Some((foreign_instance_id, false)),
                attachment_host: Some(foreign_host_id),
                applies: false,
            },
            Case {
                attachment_host: None,
                scenario: "a deleted allocation must not resume provisioning",
                purpose: NicFirmwarePurpose::Allocation {
                    instance_id: allocation_id,
                },
                attached_instance: Some((allocation_id, true)),
                applies: false,
            },
            Case {
                attachment_host: None,
                scenario: "restoration may continue for its attached deleted owner",
                purpose: NicFirmwarePurpose::RestoreBaseline {
                    instance_id: restore_owner_id,
                    cleanup_context: CleanupContext::Deprovision,
                },
                attached_instance: Some((restore_owner_id, true)),
                applies: true,
            },
            Case {
                attachment_host: None,
                scenario: "baseline completion cannot accept an attached tenant",
                purpose: NicFirmwarePurpose::Baseline {
                    continuation: NicFirmwareBaselineContinuation::Ready,
                },
                attached_instance: Some((InstanceId::new(), false)),
                applies: false,
            },
            Case {
                attachment_host: None,
                scenario: "restoration may finish after its owner was removed",
                purpose: restore.clone(),
                attached_instance: None,
                applies: true,
            },
            Case {
                attachment_host: None,
                scenario: "removed cleanup owner cannot replace a new allocation",
                purpose: restore,
                attached_instance: Some((replacement_id, false)),
                applies: false,
            },
        ];
        for (seed, case) in (30..).zip(cases) {
            let host_id = HostMachineId::try_from(machine_id(seed, MachineType::Host))?;
            let mut operation = operation();
            operation.purpose = case.purpose;
            let initial_state = ManagedHostState::NicFirmwareUpdating {
                operation: operation.clone(),
            };
            let continuation = match &operation.purpose {
                NicFirmwarePurpose::Baseline { .. } => ManagedHostState::Ready,
                NicFirmwarePurpose::Allocation { .. } => ManagedHostState::Assigned {
                    instance_state: InstanceState::DpaProvisioning,
                },
                NicFirmwarePurpose::RestoreBaseline {
                    cleanup_context, ..
                } => ManagedHostState::WaitingForCleanup {
                    cleanup_state: CleanupState::Init,
                    cleanup_context: *cleanup_context,
                },
            };
            let mut txn = pool.begin().await?;
            super::create(
                txn.as_mut(),
                None,
                &host_id.into(),
                initial_state.clone(),
                None,
                2,
            )
            .await?;
            if let Some((instance_id, deleted)) = case.attached_instance {
                let attachment_host = case.attachment_host.unwrap_or(host_id);
                if attachment_host != host_id {
                    super::create(
                        txn.as_mut(),
                        None,
                        &attachment_host.into(),
                        ManagedHostState::Ready,
                        None,
                        2,
                    )
                    .await?;
                }
                sqlx::query("INSERT INTO instances (id, machine_id, config_version, network_config, deleted) VALUES ($1, $2, $3, '{\"interfaces\": []}', $4)")
                    .bind(instance_id).bind(attachment_host).bind(config_version)
                    .bind(deleted.then(chrono::Utc::now)).execute(txn.as_mut()).await?;
            }
            let original = super::find_one(
                txn.as_mut(),
                &host_id,
                MachineSearchConfig {
                    include_history: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("host before completion");
            let new_version = original.state.version.increment();
            let prepared = super::prepare_nic_firmware_completion(
                txn.as_mut(),
                &host_id,
                original.state.version,
                &operation.purpose,
            )
            .await?;
            assert_eq!(
                prepared,
                if case.applies {
                    crate::ConditionalWrite::Applied(())
                } else {
                    crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent)
                },
                "{}",
                case.scenario,
            );
            if case.applies {
                assert_eq!(
                    super::try_update_controller_state(
                        txn.as_mut(),
                        &host_id,
                        original.state.version,
                        new_version,
                        &continuation,
                    )
                    .await?,
                    crate::ConditionalWrite::Applied(()),
                    "{}",
                    case.scenario,
                );
            }
            txn.commit().await?;
            let saved = super::find_one(
                &pool,
                &host_id,
                MachineSearchConfig {
                    include_history: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("host after completion");
            assert_eq!(
                saved.state.value,
                if case.applies {
                    continuation
                } else {
                    initial_state
                },
                "{}",
                case.scenario,
            );
            assert_eq!(
                saved.state.version,
                if case.applies {
                    new_version
                } else {
                    original.state.version
                },
                "{}",
                case.scenario,
            );
            if let Some(expected_host) = case.attachment_host {
                let saved_owner: HostMachineId =
                    sqlx::query_scalar("SELECT machine_id FROM instances WHERE id = $1")
                        .bind(foreign_instance_id)
                        .fetch_one(&pool)
                        .await?;
                assert_eq!(
                    saved_owner, expected_host,
                    "foreign ownership remains unchanged"
                );
            }
            assert_eq!(
                saved.history.len(),
                original.history.len() + usize::from(case.applies)
            );
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn host_state_lookup_preserves_association_and_decode_checks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let dpu_id: DpuMachineId =
            "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80".parse()?;
        let other_dpu_id: DpuMachineId =
            "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng".parse()?;
        let host_state = ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForPlatformConfiguration { retry_count: 3 },
        };
        let mut txn = pool.begin().await?;
        for (machine_id, state) in [
            (MachineId::from(host_id), host_state.clone()),
            (MachineId::from(dpu_id), ManagedHostState::Ready),
            (MachineId::from(other_dpu_id), ManagedHostState::Ready),
        ] {
            super::create(txn.as_mut(), None, &machine_id, state, None, 2).await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('host-state-lookup', 'V1-T0') RETURNING id",
        )
        .fetch_one(txn.as_mut())
        .await?;

        // Keep excluded interfaces present when adding the valid attachment.
        for (index, (scenario, machine_id, interface_type, expected)) in [
            (
                "self attachment",
                MachineId::from(dpu_id),
                InterfaceType::Data,
                None,
            ),
            (
                "BMC attachment",
                MachineId::from(host_id),
                InterfaceType::Bmc,
                None,
            ),
            (
                "data attachment",
                MachineId::from(host_id),
                InterfaceType::Data,
                Some(&host_state),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, segment_id,
                     mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, false, 'host-state-lookup', $5)",
            )
            .bind(machine_id)
            .bind(dpu_id)
            .bind(segment_id)
            .bind(format!("02:00:00:00:21:{index:02x}"))
            .bind(interface_type)
            .execute(txn.as_mut())
            .await?;
            let state = super::lookup_host_state_by_dpu_machine_id(txn.as_mut(), &dpu_id).await?;
            assert_eq!(state.as_ref(), expected, "{scenario}");
        }
        assert_eq!(
            super::lookup_host_state_by_dpu_machine_id(txn.as_mut(), &other_dpu_id).await?,
            None,
            "an attachment to one DPU must not match another DPU"
        );

        for (scenario, owner_id, state, column) in [
            (
                "DPU-typed owner is a decode error",
                MachineId::from(other_dpu_id),
                serde_json::to_value(ManagedHostState::Ready)?,
                "0",
            ),
            (
                "malformed host state is not a missing association",
                MachineId::from(host_id),
                serde_json::json!({}),
                "1",
            ),
        ] {
            sqlx::query(
                "UPDATE machine_interfaces SET machine_id = $1
                 WHERE attached_dpu_machine_id = $2 AND machine_id != $2
                   AND interface_type != 'Bmc'",
            )
            .bind(owner_id)
            .bind(dpu_id)
            .execute(txn.as_mut())
            .await?;
            sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
                .bind(owner_id)
                .bind(state)
                .execute(txn.as_mut())
                .await?;

            let error = super::lookup_host_state_by_dpu_machine_id(txn.as_mut(), &dpu_id)
                .await
                .expect_err(scenario);
            let crate::DatabaseError::Sqlx(annotated) = &error else {
                panic!("{scenario}: expected Sqlx, got {error:?}");
            };
            let sqlx::Error::ColumnDecode { index, .. } = &annotated.source else {
                panic!("{scenario}: expected ColumnDecode, got {error:?}");
            };
            assert_eq!(index, column, "{scenario}");
        }
        txn.rollback().await?;
        Ok(())
    }

    /// `find_dpus_by_host_machine_id` returns each DPU attached to the host through a
    /// non-BMC interface once, and restricts the snapshot to those DPUs before the
    /// per-machine enrichment runs. The lateral topology join executes once per machine
    /// that reaches it, so its loop count shows which machines were enriched.
    #[crate::sqlx_test]
    async fn find_dpus_by_host_enriches_only_the_matching_dpus(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host_id = HostMachineId::try_from(machine_id(1, MachineType::Host))?;
        let dpu_ids = [
            DpuMachineId::try_from(machine_id(2, MachineType::Dpu))?,
            DpuMachineId::try_from(machine_id(3, MachineType::Dpu))?,
        ];
        let bmc_only_dpu_id = DpuMachineId::try_from(machine_id(8, MachineType::Dpu))?;
        // Unrelated hosts and DPUs make a fleet-wide plan observable.
        let other_pairs = [
            (
                HostMachineId::try_from(machine_id(4, MachineType::Host))?,
                DpuMachineId::try_from(machine_id(5, MachineType::Dpu))?,
            ),
            (
                HostMachineId::try_from(machine_id(6, MachineType::Host))?,
                DpuMachineId::try_from(machine_id(7, MachineType::Dpu))?,
            ),
        ];

        let mut txn = pool.begin().await?;
        let fleet = [MachineId::from(host_id), MachineId::from(bmc_only_dpu_id)]
            .into_iter()
            .chain(dpu_ids.map(MachineId::from))
            .chain(
                other_pairs
                    .iter()
                    .flat_map(|(host, dpu)| [MachineId::from(*host), MachineId::from(*dpu)]),
            );
        for id in fleet {
            super::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('dpus-by-host', 'V1-T0') RETURNING id",
        )
        .fetch_one(txn.as_mut())
        .await?;
        // The first DPU is attached through two data interfaces and must be returned once;
        // the DPU attached only through a BMC interface must be excluded.
        let attachments = [
            (host_id, dpu_ids[0], InterfaceType::Data),
            (host_id, dpu_ids[0], InterfaceType::Data),
            (host_id, dpu_ids[1], InterfaceType::Data),
            (host_id, bmc_only_dpu_id, InterfaceType::Bmc),
            (other_pairs[0].0, other_pairs[0].1, InterfaceType::Data),
            (other_pairs[1].0, other_pairs[1].1, InterfaceType::Data),
        ];
        for (index, (owner_id, dpu_id, interface_type)) in attachments.into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, segment_id,
                     mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, false, 'dpus-by-host', $5)",
            )
            .bind(MachineId::from(owner_id))
            .bind(dpu_id)
            .bind(segment_id)
            .bind(format!("02:00:00:00:22:{index:02x}"))
            .bind(interface_type)
            .execute(txn.as_mut())
            .await?;
        }

        let mut found: Vec<DpuMachineId> =
            super::find_dpus_by_host_machine_id(txn.as_mut(), &host_id)
                .await?
                .into_iter()
                .map(|dpu| dpu.id)
                .collect();
        found.sort();
        let mut expected = dpu_ids.to_vec();
        expected.sort();
        assert_eq!(found, expected);

        fn actual_loops(node: &serde_json::Value, relation: &str, loops: &mut Vec<u64>) {
            if node["Relation Name"] == relation {
                loops.push(node["Actual Loops"].as_u64().unwrap_or_default());
            }
            for child in node["Plans"].as_array().into_iter().flatten() {
                actual_loops(child, relation, loops);
            }
        }
        let explain = format!(
            "EXPLAIN (ANALYZE, FORMAT JSON) {}",
            super::FIND_DPUS_BY_HOST_MACHINE_ID_QUERY.as_str()
        );
        let sqlx::types::Json(plan): sqlx::types::Json<serde_json::Value> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(explain.as_str()))
                .bind(host_id)
                .fetch_one(txn.as_mut())
                .await?;
        let mut loops = Vec::new();
        actual_loops(&plan[0]["Plan"], "machine_topologies", &mut loops);
        assert_eq!(loops, vec![dpu_ids.len() as u64], "{plan:#}");

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn health_alert_search_uses_source_keys(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let source = "future.source with 'quotes' and \"double quotes\"";
        let populated = serde_json::json!({
            "merges": {
                source: {"alerts": [{"id": "leak", "message": "nested payload"}]},
                "empty": {"alerts": []}
            }
        });
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        sqlx::query("UPDATE machines SET health_reports = $1 WHERE id = $2")
            .bind(&populated)
            .bind(machine_id)
            .execute(txn.as_mut())
            .await?;
        txn.commit().await?;

        // Reconstruct the predecessor index with populated data, then apply the
        // shipped replacement outside a transaction, as the migrator does.
        sqlx::raw_sql(
            "DROP INDEX machine_health_source_keys_gin_idx;
             CREATE INDEX machine_health_reports_merges_gin_idx ON machines
             USING gin ((health_reports->'merges'));",
        )
        .execute(&pool)
        .await?;
        for migration in [
            include_str!("../migrations/20261013175247_machine_health_source_keys_gin.sql"),
            include_str!("../migrations/20261013175248_drop_machine_health_reports_merges_gin.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await?;
        }

        for (scenario, reports, searched_source, expected) in [
            ("arbitrary source", Some(populated.clone()), source, true),
            ("empty alerts", Some(populated.clone()), "empty", false),
            (
                "nested value is not a source",
                Some(populated),
                "leak",
                false,
            ),
            ("absent reports", None, source, false),
            ("absent merges", Some(serde_json::json!({})), source, false),
        ] {
            sqlx::query("UPDATE machines SET health_reports = $1 WHERE id = $2")
                .bind(reports)
                .bind(machine_id)
                .execute(&pool)
                .await?;
            let ids: Vec<HostMachineId> = super::find_machine_ids(
                &pool,
                MachineSearchConfig {
                    only_with_health_alert: Some(searched_source.to_string()),
                    ..Default::default()
                },
            )
            .await?;
            assert_eq!(ids.contains(&machine_id), expected, "{scenario}");
        }

        // A generic prepared plan must match the expression index even when
        // the report source is supplied as a parameter.
        let mut txn = pool.begin().await?;
        sqlx::raw_sql(
            "SET LOCAL enable_seqscan = off; SET LOCAL plan_cache_mode = force_generic_plan",
        )
        .execute(txn.as_mut())
        .await?;
        let _: Vec<HostMachineId> = super::find_machine_ids(
            txn.as_mut(),
            MachineSearchConfig {
                only_with_health_alert: Some(source.to_string()),
                ..Default::default()
            },
        )
        .await?;
        let statement: String = sqlx::query_scalar(
            "SELECT statement FROM pg_prepared_statements
             WHERE statement LIKE 'SELECT id FROM machines%'
             AND statement LIKE '%jsonb_array_length%'",
        )
        .fetch_one(txn.as_mut())
        .await?;
        let plan: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {statement}")))
                .bind(source)
                .bind(source)
                .fetch_all(txn.as_mut())
                .await?;
        assert!(
            plan.iter()
                .any(|line| line.contains("machine_health_source_keys_gin_idx")),
            "source-key index missing from plan: {plan:?}"
        );
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn dpu_agent_upgrade_status_reads_current_attached_host_data(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let host_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let dpu_id: DpuMachineId =
            "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80".parse()?;
        let mut txn = pool.begin().await?;
        assert!(
            super::find_dpu_agent_upgrade_status(txn.as_mut(), &dpu_id)
                .await?
                .is_none()
        );
        for machine_id in [MachineId::from(host_id), MachineId::from(dpu_id)] {
            super::create(
                txn.as_mut(),
                None,
                &machine_id,
                ManagedHostState::Ready,
                None,
                2,
            )
            .await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('agent-upgrade-status', 'V1-T0') RETURNING id",
        )
        .fetch_one(txn.as_mut())
        .await?;

        // Add one interface at a time. BMC and self links must not attach a host;
        // multiple data interfaces on that host are a valid attachment.
        for (index, (scenario, machine_id, interface_type, attached)) in [
            ("self link", MachineId::from(dpu_id), "Data", false),
            ("BMC link", MachineId::from(host_id), "Bmc", false),
            ("data link", MachineId::from(host_id), "Data", true),
            ("second data link", MachineId::from(host_id), "Data", true),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type, segment_id,
                     mac_address, primary_interface, hostname, interface_type)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, false, 'upgrade-status',
                         $5::text::interface_type)",
            )
            .bind(machine_id)
            .bind(dpu_id)
            .bind(segment_id)
            .bind(format!("02:00:00:00:20:{index:02x}"))
            .bind(interface_type)
            .execute(txn.as_mut())
            .await?;
            let status = super::find_dpu_agent_upgrade_status(txn.as_mut(), &dpu_id).await?;
            assert_eq!(status.is_some(), attached, "{scenario}");
        }
        txn.commit().await?;

        let mut reader = pool.begin().await?;
        let status = super::find_dpu_agent_upgrade_status(reader.as_mut(), &dpu_id)
            .await?
            .expect("data interfaces attach the DPU");
        assert_eq!(status.id, dpu_id);
        assert_eq!(status.agent_version, None);
        assert!(!status.upgrade_requested);
        assert!(!status.host_dpf_used_for_ingestion);
        reader.commit().await?;

        let mut writer = pool.begin().await?;
        let observation = model::machine::network::MachineNetworkStatusObservation {
            machine_id: dpu_id,
            agent_version: Some("v2026.10".to_string()),
            observed_at: chrono::Utc::now(),
            network_config_version: None,
            client_certificate_expiry: None,
            agent_version_superseded_at: None,
            instance_network_observation: None,
            fabric_interfaces: Vec::new(),
            accepted_lldp_snapshot_fingerprint: None,
        };
        assert!(matches!(
            super::update_network_status_observation(writer.as_mut(), &dpu_id, &observation)
                .await?,
            crate::ConditionalWrite::Applied(())
        ));
        super::set_dpu_agent_upgrade_requested(writer.as_mut(), &dpu_id, true, "v2026.11").await?;
        super::mark_machine_ingestion_done_with_dpf(writer.as_mut(), &host_id).await?;
        writer.commit().await?;

        let mut reader = pool.begin().await?;
        let status = super::find_dpu_agent_upgrade_status(reader.as_mut(), &dpu_id)
            .await?
            .expect("data interfaces still attach the DPU");
        assert_eq!(status.agent_version.as_deref(), Some("v2026.10"));
        assert!(status.upgrade_requested);
        assert!(status.host_dpf_used_for_ingestion);
        reader.commit().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn spx_multiplane_initializes_on_host_creation_only(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::expected_machine::{ExpectedInterface, ExpectedMachineData};
        let data = ExpectedMachineData {
            interfaces: vec![ExpectedInterface {
                logical_ifname: Some("eth_r2_p6".to_string()),
                physical_ifname: Some("C5-1-L3".to_string()),
                nic_type: Some("CX9".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        for (id, expected) in [
            (
                "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30",
                true,
            ),
            (
                "fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0",
                false,
            ),
        ] {
            let mut txn = pool.begin().await?;
            let machine = super::create(
                &mut txn,
                None,
                &id.parse::<MachineId>()?,
                ManagedHostState::Ready,
                Some(&data),
                2,
            )
            .await?;
            assert_eq!(machine.spx_multiplane.is_some(), expected);
            if let Some(configuration) = machine.spx_multiplane {
                assert!(
                    configuration.interfaces["eth_r2_p6"]
                        .cfg_maintenance_mode
                        .is_none()
                );
            }
            txn.commit().await?;
        }
        Ok(())
    }

    /// A retry may opt in after an earlier request omitted the option, but later
    /// requests cannot cancel a wait once it has been recorded.
    #[crate::sqlx_test]
    async fn force_delete_ack_requirement_only_becomes_stricter(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::ForceDeletion,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        for (requested, expected) in [(false, false), (true, true), (false, true)] {
            let mut txn = pool.begin().await?;
            super::find_one(
                txn.as_mut(),
                &machine_id,
                MachineSearchConfig {
                    for_update: true,
                    ..Default::default()
                },
            )
            .await?
            .expect("fixture host must remain available");
            assert_eq!(
                super::record_force_delete_admin_ack_requirement(
                    txn.as_mut(),
                    &machine_id,
                    requested,
                )
                .await?,
                expected,
            );
            txn.commit().await?;
            let stored: Option<bool> = sqlx::query_scalar(
                "SELECT force_delete_requires_admin_ack FROM machines WHERE id = $1",
            )
            .bind(machine_id)
            .fetch_one(&pool)
            .await?;
            assert_eq!(stored, Some(expected));
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn machine_observations_compare_timestamps_chronologically(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::infiniband::MachineInfinibandStatusObservation;
        use model::machine::nvlink::MachineNvLinkStatusObservation;
        use model::machine::spx::MachineSpxStatusObservation;
        use model::machine::status::MlxDeviceObservation;

        use super::{
            MachineObservationNotCurrent, update_infiniband_status_observation,
            update_mlx_device_observation, update_nvlink_status_observation,
            update_spx_status_observation,
        };
        use crate::ConditionalWrite::{Applied, NotApplied};

        struct Case {
            scenario: &'static str,
            stored_at: Option<&'static str>,
            observed_at: &'static str,
            applies: bool,
        }

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut txn = pool.begin().await?;
        super::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        // Each column has its own SQL predicate. Check persisted JSON, not only
        // the result, so a rejected observation cannot replace newer data.
        // Only the literal column names below are interpolated into SQL.
        for column in [
            "infiniband_status_observation",
            "nvlink_status_observation",
            "spx_status_observation",
            "mlx_device_observation",
        ] {
            for Case {
                scenario,
                stored_at,
                observed_at,
                applies,
            } in [
                Case {
                    scenario: "first observation",
                    stored_at: None,
                    observed_at: "2026-09-16T16:00:00.123Z",
                    applies: true,
                },
                Case {
                    scenario: "newer with more fractional digits",
                    stored_at: Some("2026-09-16T16:00:00.123Z"),
                    observed_at: "2026-09-16T16:00:00.123001+00:00",
                    applies: true,
                },
                Case {
                    scenario: "equal nanosecond timestamp",
                    stored_at: Some("2026-09-16T16:00:00.123456789Z"),
                    observed_at: "2026-09-16T16:00:00.123456789+00:00",
                    applies: true,
                },
                Case {
                    scenario: "older timestamp with a different offset",
                    stored_at: Some("2026-09-16T15:00:00.123001-01:00"),
                    observed_at: "2026-09-16T16:00:00.123Z",
                    applies: false,
                },
            ] {
                let stored =
                    stored_at.map(|timestamp| serde_json::json!({"observed_at": timestamp}));
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE machines SET {column} = $1 WHERE id = $2"
                )))
                .bind(stored.as_ref().map(sqlx::types::Json))
                .bind(machine_id)
                .execute(txn.as_mut())
                .await?;
                let observed_at = observed_at.parse()?;
                let (result, mut incoming) = match column {
                    "infiniband_status_observation" => {
                        let observation = MachineInfinibandStatusObservation {
                            observed_at,
                            ib_interfaces: Vec::new(),
                        };
                        (
                            update_infiniband_status_observation(
                                txn.as_mut(),
                                &machine_id,
                                &observation,
                            )
                            .await?,
                            serde_json::to_value(observation)?,
                        )
                    }
                    "nvlink_status_observation" => {
                        let observation = MachineNvLinkStatusObservation {
                            observed_at,
                            nvlink_gpus: Vec::new(),
                        };
                        (
                            update_nvlink_status_observation(
                                txn.as_mut(),
                                &machine_id,
                                &observation,
                            )
                            .await?,
                            serde_json::to_value(observation)?,
                        )
                    }
                    "spx_status_observation" => {
                        let observation = MachineSpxStatusObservation {
                            observed_at,
                            spx_attachments: Vec::new(),
                        };
                        (
                            update_spx_status_observation(txn.as_mut(), &machine_id, &observation)
                                .await?,
                            serde_json::to_value(observation)?,
                        )
                    }
                    "mlx_device_observation" => {
                        let observation = MlxDeviceObservation {
                            observed_at,
                            received_at: None,
                            devices: Vec::new(),
                        };
                        (
                            update_mlx_device_observation(txn.as_mut(), &machine_id, &observation)
                                .await?,
                            serde_json::to_value(observation)?,
                        )
                    }
                    _ => unreachable!("the table contains only observation columns"),
                };
                assert_eq!(
                    result,
                    if applies {
                        Applied(())
                    } else {
                        NotApplied(MachineObservationNotCurrent)
                    },
                    "{column}: {scenario}"
                );
                let persisted: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(
                    format!("SELECT {column} FROM machines WHERE id = $1"),
                ))
                .bind(machine_id)
                .fetch_one(txn.as_mut())
                .await?;
                // An accepted MLX report also gains the database's receipt time,
                // which the incoming report cannot know in advance.
                if column == "mlx_device_observation" && applies {
                    let _: chrono::DateTime<chrono::Utc> =
                        serde_json::from_value(persisted["received_at"].clone())?;
                    incoming["received_at"] = persisted["received_at"].clone();
                }
                assert_eq!(
                    persisted,
                    if applies { incoming } else { stored.unwrap() },
                    "{column}: {scenario}"
                );
            }
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn clearing_nvlink_observations_writes_only_rows_holding_one(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use chrono::{DateTime, Utc};

        use super::clear_nvlink_status_observations;

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut txn = pool.begin().await?;
        super::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        // The `updated` trigger stamps NOW(), which is constant inside one
        // transaction, so every step commits before the row is read back.
        async fn clear_in_own_transaction(
            pool: &sqlx::PgPool,
            machine_id: HostMachineId,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let mut txn = pool.begin().await?;
            clear_nvlink_status_observations(txn.as_mut(), &[machine_id]).await?;
            txn.commit().await?;
            Ok(())
        }
        async fn row(
            pool: &sqlx::PgPool,
            machine_id: &HostMachineId,
        ) -> Result<(Option<serde_json::Value>, DateTime<Utc>), Box<dyn std::error::Error>>
        {
            Ok(sqlx::query_as(
                "SELECT nvlink_status_observation, updated FROM machines WHERE id = $1",
            )
            .bind(machine_id)
            .fetch_one(pool)
            .await?)
        }

        let (_, updated_before) = row(&pool, &machine_id).await?;
        clear_in_own_transaction(&pool, machine_id).await?;
        let (observation, updated_after) = row(&pool, &machine_id).await?;
        assert_eq!(observation, None);
        assert_eq!(
            updated_after, updated_before,
            "a row already holding NULL must not be rewritten"
        );

        sqlx::query("UPDATE machines SET nvlink_status_observation = $1 WHERE id = $2")
            .bind(sqlx::types::Json(
                serde_json::json!({"observed_at": "2026-09-16T16:00:00Z", "nvlink_gpus": []}),
            ))
            .bind(machine_id)
            .execute(&pool)
            .await?;
        let (observation, updated_before) = row(&pool, &machine_id).await?;
        assert!(observation.is_some());
        clear_in_own_transaction(&pool, machine_id).await?;
        let (observation, updated_after) = row(&pool, &machine_id).await?;
        assert_eq!(observation, None);
        assert!(
            updated_after > updated_before,
            "a cleared row is a new row version"
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn mlx_observation_round_trips_through_machine_snapshot(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::status::MlxDeviceObservation;

        use super::{MachineObservationNotCurrent, update_mlx_device_observation};
        use crate::ConditionalWrite::{Applied, NotApplied};

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let legacy_json = serde_json::json!({
            "observed_at": "2026-09-16T16:00:00.123456789Z",
            "devices": [
                {
                    "pci_name": "0000:01:00.0",
                    "device_type": "ConnectX8",
                    "psid": "MT_0000001111",
                    "part_number": "MCX813106AS-VEA",
                    "fw_version_current": "40.45.1000"
                },
                {
                    "pci_name": "0000:b4:00.0",
                    "device_type": "BlueField3",
                    "status": "Failed to open device"
                }
            ]
        });
        let mut observation: MlxDeviceObservation = serde_json::from_value(legacy_json.clone())?;
        assert_eq!(observation.received_at, None);
        let mut txn = pool.begin().await?;
        assert_eq!(
            update_mlx_device_observation(txn.as_mut(), &machine_id, &observation).await?,
            NotApplied(MachineObservationNotCurrent),
            "a missing machine cannot accept an observation"
        );
        let machine = super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        assert_eq!(machine.status.mlx_device_observation, None);

        // Historical JSON has no receipt time. Loading it must not invent one
        // that could satisfy a later controller activation boundary.
        sqlx::query("UPDATE machines SET mlx_device_observation = $1 WHERE id = $2")
            .bind(sqlx::types::Json(legacy_json))
            .bind(machine_id)
            .execute(txn.as_mut())
            .await?;
        let legacy = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("legacy observation host");
        assert_eq!(
            legacy.status.mlx_device_observation,
            Some(observation.clone())
        );

        // Only the persistence boundary assigns receipt time, independently of
        // both the Scout timestamp and any value supplied by an internal caller.
        observation.received_at = Some("2099-01-01T00:00:00Z".parse()?);
        let before: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(txn.as_mut())
            .await?;
        assert_eq!(
            update_mlx_device_observation(txn.as_mut(), &machine_id, &observation).await?,
            Applied(())
        );
        let after: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(txn.as_mut())
            .await?;
        txn.commit().await?;

        let machine = super::find_one(&pool, &machine_id, MachineSearchConfig::default())
            .await?
            .expect("fixture host");
        let saved = machine
            .status
            .mlx_device_observation
            .expect("stored report");
        assert_eq!(saved.observed_at, observation.observed_at);
        assert_eq!(saved.devices, observation.devices);
        let received_at = saved.received_at.expect("database receipt time");
        assert!(received_at >= before && received_at <= after);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn concurrent_mlx_observations_preserve_newer_report(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use model::machine::status::MlxDeviceObservation;

        use super::{MachineObservationNotCurrent, update_mlx_device_observation};
        use crate::ConditionalWrite::{Applied, NotApplied};

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        let newer = MlxDeviceObservation {
            observed_at: "2026-09-16T16:00:01Z".parse()?,
            received_at: None,
            devices: Vec::new(),
        };
        let older = MlxDeviceObservation {
            observed_at: "2026-09-16T16:00:00Z".parse()?,
            received_at: None,
            devices: Vec::new(),
        };
        let mut newer_txn = pool.begin().await?;
        assert_eq!(
            update_mlx_device_observation(newer_txn.as_mut(), &machine_id, &newer).await?,
            Applied(())
        );
        let accepted = super::find_one(
            newer_txn.as_mut(),
            &machine_id,
            MachineSearchConfig::default(),
        )
        .await?
        .expect("newer observation host")
        .status
        .mlx_device_observation
        .expect("accepted report");
        assert!(accepted.received_at.is_some());
        let mut older_txn = pool.begin().await?;
        let release_newer = async {
            // The older write must reach the locked row before the newer
            // report commits, so its predicate must be rechecked after waiting.
            wait_until_blocked_on(&pool, "UPDATE machines SET mlx_device_observation", 1).await;
            newer_txn.commit().await
        };
        let (older_result, released) =
            tokio::time::timeout(std::time::Duration::from_secs(90), async {
                tokio::join!(
                    update_mlx_device_observation(older_txn.as_mut(), &machine_id, &older),
                    release_newer,
                )
            })
            .await
            .expect("the older observation must finish after the newer transaction commits");
        released?;
        assert_eq!(older_result?, NotApplied(MachineObservationNotCurrent));
        older_txn.commit().await?;

        let machine = super::find_one(&pool, &machine_id, MachineSearchConfig::default())
            .await?
            .expect("fixture host");
        assert_eq!(machine.status.mlx_device_observation, Some(accepted));
        Ok(())
    }

    #[crate::sqlx_test]
    async fn network_observations_compare_fractional_timestamps(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::DpuMachineId;
        use chrono::{DateTime, Duration, Utc};
        use model::machine::network::MachineNetworkStatusObservation;

        use super::{MachineObservationNotCurrent, update_network_status_observation};
        use crate::ConditionalWrite::{self, Applied, NotApplied};

        let machine_id: DpuMachineId =
            "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng".parse()?;
        let observed_at = DateTime::from_timestamp(1_722_000_000, 123_456_789).unwrap();
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        struct Case {
            scenario: &'static str,
            observed_at: DateTime<Utc>,
            expect: ConditionalWrite<(), MachineObservationNotCurrent>,
        }
        let mut expected = None;
        for case in [
            Case {
                scenario: "first observation",
                observed_at,
                expect: Applied(()),
            },
            Case {
                scenario: "equal timestamp replaces the payload",
                observed_at,
                expect: Applied(()),
            },
            Case {
                scenario: "older observation preserves the payload",
                observed_at: observed_at - Duration::microseconds(1),
                expect: NotApplied(MachineObservationNotCurrent),
            },
        ] {
            let observation = MachineNetworkStatusObservation {
                machine_id,
                agent_version: Some(case.scenario.to_string()),
                observed_at: case.observed_at,
                network_config_version: None,
                client_certificate_expiry: None,
                agent_version_superseded_at: None,
                instance_network_observation: None,
                fabric_interfaces: Vec::new(),
                accepted_lldp_snapshot_fingerprint: None,
            };
            let mut txn = pool.begin().await?;
            let result =
                update_network_status_observation(txn.as_mut(), &machine_id, &observation).await?;
            txn.commit().await?;
            assert_eq!(result, case.expect, "{}", case.scenario);
            if let Applied(()) = case.expect {
                expected = Some(observation);
            }
            let persisted: sqlx::types::Json<MachineNetworkStatusObservation> =
                sqlx::query_scalar("SELECT network_status_observation FROM machines WHERE id = $1")
                    .bind(machine_id)
                    .fetch_one(&pool)
                    .await?;
            assert_eq!(Some(persisted.0), expected, "{}", case.scenario);
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn network_observation_update_preserves_server_owned_lldp_fingerprint(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::DpuMachineId;
        use chrono::{DateTime, Duration, Utc};
        use model::machine::network::MachineNetworkStatusObservation;

        use super::{update_accepted_lldp_snapshot_fingerprint, update_network_status_observation};
        use crate::ConditionalWrite::Applied;

        let machine_id: DpuMachineId =
            "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng".parse()?;
        let observed_at = DateTime::<Utc>::from_timestamp(1_722_000_000, 0).unwrap();
        let fingerprint_a = vec![0x0a; 32];
        let fingerprint_b = vec![0x0b; 32];

        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        let initial = MachineNetworkStatusObservation {
            machine_id,
            agent_version: Some("initial-agent".to_string()),
            observed_at,
            network_config_version: None,
            client_certificate_expiry: None,
            agent_version_superseded_at: None,
            instance_network_observation: None,
            fabric_interfaces: Vec::new(),
            accepted_lldp_snapshot_fingerprint: None,
        };
        assert_eq!(
            update_network_status_observation(txn.as_mut(), &machine_id, &initial).await?,
            Applied(())
        );
        update_accepted_lldp_snapshot_fingerprint(txn.as_mut(), &machine_id, &fingerprint_a)
            .await?;
        txn.commit().await?;

        let newer = MachineNetworkStatusObservation {
            machine_id,
            agent_version: Some("newer-agent".to_string()),
            observed_at: observed_at + Duration::seconds(1),
            network_config_version: None,
            client_certificate_expiry: None,
            agent_version_superseded_at: None,
            instance_network_observation: None,
            fabric_interfaces: Vec::new(),
            accepted_lldp_snapshot_fingerprint: Some(fingerprint_b),
        };
        let mut txn = pool.begin().await?;
        assert_eq!(
            update_network_status_observation(txn.as_mut(), &machine_id, &newer).await?,
            Applied(())
        );
        txn.commit().await?;

        let persisted: sqlx::types::Json<MachineNetworkStatusObservation> =
            sqlx::query_scalar("SELECT network_status_observation FROM machines WHERE id = $1")
                .bind(machine_id)
                .fetch_one(&pool)
                .await?;
        let persisted = persisted.0;
        assert_eq!(persisted.agent_version.as_deref(), Some("newer-agent"));
        assert_eq!(
            persisted.accepted_lldp_snapshot_fingerprint,
            Some(fingerprint_a)
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn extension_service_observations_preserve_per_service_timestamp_order(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use chrono::{DateTime, Utc};
        use config_version::ConfigVersion;
        use model::extension_service::ExtensionServiceType;
        use model::instance::status::extension_service::{
            InstanceExtensionServiceStatusObservation,
            InstanceExtensionServiceStatusObservationByType,
        };

        use super::{
            ExtensionServiceObservationNotCurrent, update_extension_service_status_observation,
        };
        use crate::ConditionalWrite::{self, Applied, NotApplied};

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        let observation = InstanceExtensionServiceStatusObservation {
            config_version: ConfigVersion::initial(),
            instance_config_version: None,
            extension_service_statuses: Vec::new(),
            observed_at: DateTime::from_timestamp(1_722_000_000, 123_456_789).unwrap(),
        };
        let mut txn = pool.begin().await?;
        let missing = update_extension_service_status_observation(
            txn.as_mut(),
            &machine_id,
            ExtensionServiceType::KubernetesPod,
            &observation,
        )
        .await;
        assert!(
            matches!(missing, Err(crate::DatabaseError::NotFoundError { kind: "machine", id }) if id == machine_id.to_string())
        );
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        struct Case {
            scenario: &'static str,
            service_type: ExtensionServiceType,
            observed_at: DateTime<Utc>,
            config_version: ConfigVersion,
            expect: ConditionalWrite<(), ExtensionServiceObservationNotCurrent>,
        }
        let mut expected = InstanceExtensionServiceStatusObservationByType::default();
        for case in [
            Case {
                scenario: "initial observation",
                service_type: ExtensionServiceType::KubernetesPod,
                observed_at: observation.observed_at,
                config_version: observation.config_version,
                expect: Applied(()),
            },
            Case {
                scenario: "another service has an independent timestamp",
                service_type: ExtensionServiceType::DpfHelmChart,
                observed_at: observation.observed_at - chrono::Duration::seconds(1),
                config_version: observation.config_version,
                expect: Applied(()),
            },
            Case {
                scenario: "older observation leaves both services unchanged",
                service_type: ExtensionServiceType::KubernetesPod,
                observed_at: observation.observed_at - chrono::Duration::seconds(1),
                config_version: observation.config_version.increment(),
                expect: NotApplied(ExtensionServiceObservationNotCurrent),
            },
            Case {
                scenario: "equal timestamp can replace the payload",
                service_type: ExtensionServiceType::KubernetesPod,
                observed_at: observation.observed_at,
                config_version: observation.config_version.increment(),
                expect: Applied(()),
            },
        ] {
            let incoming = InstanceExtensionServiceStatusObservation {
                config_version: case.config_version,
                observed_at: case.observed_at,
                ..observation.clone()
            };
            let mut txn = pool.begin().await?;
            let result = update_extension_service_status_observation(
                txn.as_mut(),
                &machine_id,
                case.service_type.clone(),
                &incoming,
            )
            .await?;
            txn.commit().await?;
            assert_eq!(result, case.expect, "{}", case.scenario);
            if let Applied(()) = case.expect {
                expected.set_for_service_type(case.service_type, incoming);
            }
            let persisted: sqlx::types::Json<InstanceExtensionServiceStatusObservationByType> =
                sqlx::query_scalar(
                    "SELECT extension_service_status_observations FROM machines WHERE id = $1",
                )
                .bind(machine_id)
                .fetch_one(&pool)
                .await?;
            assert_eq!(persisted.0, expected, "{}", case.scenario);
        }
        Ok(())
    }

    fn common_pools_without_seeded_values() -> CommonPools {
        CommonPools {
            ethernet: EthernetPools {
                pool_loopback_ip: Arc::new(ResourcePool::new(
                    LOOPBACK_IP.to_string(),
                    ValueType::Ipv4,
                )),
                pool_loopback_ip_v6: Arc::new(ResourcePool::new(
                    LOOPBACK_IP_V6.to_string(),
                    ValueType::Ipv6,
                )),
                pool_vlan_id: Arc::new(ResourcePool::new(VLANID.to_string(), ValueType::Integer)),
                pool_vni: Arc::new(ResourcePool::new(VNI.to_string(), ValueType::Integer)),
                pool_vpc_vni: Arc::new(ResourcePool::new(VPC_VNI.to_string(), ValueType::Integer)),
                pool_external_vpc_vni: Arc::new(ResourcePool::new(
                    EXTERNAL_VPC_VNI.to_string(),
                    ValueType::Integer,
                )),
                pool_dpa_vni: Arc::new(ResourcePool::new(DPA_VNI.to_string(), ValueType::Integer)),
                pool_fnn_asn: Arc::new(ResourcePool::new(FNN_ASN.to_string(), ValueType::Integer)),
                pool_vpc_dpu_loopback_ip: Arc::new(ResourcePool::new(
                    VPC_DPU_LOOPBACK.to_string(),
                    ValueType::Ipv4,
                )),
            },
            infiniband: IbPools::default(),
        }
    }

    #[crate::sqlx_test]
    async fn syncing_stable_id_cascades_dpa_interfaces_to_stable_id(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use mac_address::MacAddress;
        use model::dpa_interface::{DpaInterfaceType, DpaSearchConfig, NewDpaInterface};

        let predicted_id = HostMachineId::try_from(machine_id(1, MachineType::PredictedHost))?;
        let stable_id = StableHostMachineId::try_from(machine_id(1, MachineType::Host))?;

        let mut txn = pool.begin().await?;

        super::create(
            txn.as_mut(),
            None,
            &predicted_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        for mac in ["00:11:22:33:44:55", "00:11:22:33:44:66"] {
            crate::dpa_interface::persist(
                NewDpaInterface {
                    mac_address: MacAddress::from_str(mac)?,
                    machine_id: predicted_id,
                    device_type: "Bluefield 3".to_string(),
                    pci_name: "5e:00.0".to_string(),
                    device_description: None,
                    interface_type: DpaInterfaceType::Svpc,
                },
                &mut txn,
            )
            .await?;
        }

        // Renaming the predicted host must succeed even though dpa_interfaces
        // rows reference the old id: the ON UPDATE CASCADE FK moves them.
        let renamed = super::try_sync_stable_id_with_current_machine_id_for_host(
            &mut txn,
            Some(predicted_id),
            &stable_id,
        )
        .await?;
        assert_eq!(renamed, stable_id);

        let under_predicted = crate::dpa_interface::find_by_machine_id(
            txn.as_mut(),
            predicted_id,
            DpaSearchConfig::default(),
        )
        .await?;
        assert!(
            under_predicted.is_empty(),
            "dpa_interfaces should no longer reference the predicted id",
        );

        let under_stable = crate::dpa_interface::find_by_machine_id(
            txn.as_mut(),
            stable_id.to_host_machine_id(),
            DpaSearchConfig::default(),
        )
        .await?;
        assert_eq!(
            under_stable.len(),
            2,
            "both dpa_interfaces rows should cascade to the stable id",
        );

        Ok(())
    }

    /// The pre-lock holds every interface row of the host, BMC row included,
    /// and its machines row, and no other machine's rows, before the rename
    /// rewrites them.
    #[crate::sqlx_test]
    async fn lock_rows_for_stable_id_sync_holds_the_host_interface_and_machines_rows(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use carbide_uuid::machine::{MachineIdSource, MachineType, PredictedHostMachineId};

        fn machine_id(marker: u8, machine_type: MachineType) -> MachineId {
            let mut hardware_id = [0u8; 32];
            hardware_id[0] = marker;
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                hardware_id,
                machine_type,
            )
        }

        /// Whether `query`, a `FOR UPDATE NOWAIT` of the row with the text
        /// id `id`, fails because another transaction holds the row.
        async fn row_is_held(pool: &sqlx::PgPool, query: &'static str, id: &str) -> bool {
            let mut probe = pool.begin().await.unwrap();
            let result = sqlx::query(query).bind(id).fetch_one(probe.as_mut()).await;
            probe.rollback().await.unwrap();
            match result {
                Ok(_) => false,
                Err(error) => {
                    assert_eq!(
                        error
                            .as_database_error()
                            .and_then(sqlx::error::DatabaseError::code)
                            .as_deref(),
                        Some("55P03"),
                        "{error}"
                    );
                    true
                }
            }
        }
        const INTERFACE_HELD: &str =
            "SELECT id FROM machine_interfaces WHERE id = $1::uuid FOR UPDATE NOWAIT";
        const MACHINE_HELD: &str = "SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT";

        let predicted_id =
            PredictedHostMachineId::try_from(machine_id(1, MachineType::PredictedHost))?;
        let stable_id = StableHostMachineId::try_from(machine_id(1, MachineType::Host))?;
        let other_machine_id = machine_id(2, MachineType::Host);

        let mut setup = pool.begin().await?;
        for id in [MachineId::from(predicted_id), other_machine_id] {
            super::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        }
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ('stable-id-sync', 'V1-T0')
             RETURNING id",
        )
        .fetch_one(setup.as_mut())
        .await?;
        let mut interface_ids = Vec::new();
        for (index, (owner, interface_type)) in [
            (MachineId::from(predicted_id), "Data"),
            (MachineId::from(predicted_id), "Bmc"),
            (MachineId::from(predicted_id), "Data"),
            (other_machine_id, "Data"),
        ]
        .into_iter()
        .enumerate()
        {
            let interface_id: MachineInterfaceId = sqlx::query_scalar(
                "INSERT INTO machine_interfaces
                     (machine_id, association_type, segment_id, mac_address,
                      primary_interface, hostname, interface_type)
                 VALUES ($1, 'Machine', $2, $3::macaddr, false, $4, $5::interface_type)
                 RETURNING id",
            )
            .bind(owner)
            .bind(segment_id)
            .bind(format!("02:00:00:00:00:0{index}"))
            .bind(format!("stable-id-sync-{index}"))
            .bind(interface_type)
            .fetch_one(setup.as_mut())
            .await?;
            interface_ids.push(interface_id);
        }
        let other_interface_id = interface_ids.pop().expect("four rows were inserted");
        setup.commit().await?;

        let mut txn = pool.begin().await?;
        super::lock_rows_for_stable_id_sync(&mut txn, &predicted_id).await?;

        for interface_id in &interface_ids {
            assert!(
                row_is_held(&pool, INTERFACE_HELD, &interface_id.to_string()).await,
                "interface row {interface_id} of the predicted host should be held"
            );
        }
        assert!(
            row_is_held(&pool, MACHINE_HELD, &predicted_id.to_string()).await,
            "the machines row of the predicted host should be held"
        );
        assert!(
            !row_is_held(&pool, INTERFACE_HELD, &other_interface_id.to_string()).await,
            "another machine's interface row should be free"
        );

        let renamed = super::try_sync_stable_id_with_current_machine_id_for_host(
            &mut txn,
            Some(HostMachineId::from(predicted_id)),
            &stable_id,
        )
        .await?;
        assert_eq!(renamed, stable_id);
        let mut renamed_rows: Vec<MachineInterfaceId> =
            sqlx::query_scalar("SELECT id FROM machine_interfaces WHERE machine_id = $1")
                .bind(stable_id)
                .fetch_all(txn.as_mut())
                .await?;
        renamed_rows.sort();
        interface_ids.sort();
        assert_eq!(renamed_rows, interface_ids);
        txn.rollback().await?;

        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_reset_claim_serializes_with_allocation(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut setup = pool.begin().await?;
        super::create(
            setup.as_mut(),
            None,
            &machine_id.into(),
            ManagedHostState::Ready,
            None,
            super::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        setup.commit().await?;

        let mut allocation = pool.begin().await?;
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(allocation.as_mut())
            .await?;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
            .bind(machine_id)
            .execute(allocation.as_mut())
            .await?;
        sqlx::query("INSERT INTO instances (machine_id) VALUES ($1)")
            .bind(machine_id)
            .execute(allocation.as_mut())
            .await?;
        let claim = async {
            let mut txn = pool.begin().await?;
            let claimed = super::try_set_machine_maintenance_requested(
                txn.as_mut(),
                machine_id,
                "rack-test",
                super::MachineMaintenanceOperation::Reset { graceful: false },
            )
            .await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(claimed)
        };
        let commit_allocation = async {
            crate::test_support::postgres::wait_for_blocked_query(&pool, blocker, "FOR UPDATE")
                .await;
            allocation.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (claimed, ()) = tokio::time::timeout(std::time::Duration::from_secs(40), async {
            tokio::try_join!(claim, commit_allocation)
        })
        .await??;
        assert!(
            !claimed,
            "a committed instance must block Reset even while the host is Ready"
        );
        let pending: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT machine_maintenance_requested FROM machines WHERE id = $1")
                .bind(machine_id)
                .fetch_one(&pool)
                .await?;
        assert!(pending.is_none());
        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_reset_admission_and_cancellation_use_machine_boundaries(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use chrono::Utc;
        use config_version::ConfigVersion;
        use model::machine::{
            FailureCause, FailureDetails, FailureSource, HostReprovisionState, MachineState,
            ReadyBootConfigState,
        };
        use model::machine_boot_interface::MachineBootInterfaceTarget;

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let boot = |boot_config_state| ManagedHostState::BootConfiguring {
            desired_version: ConfigVersion::initial(),
            desired_boot_interface: MachineBootInterfaceTarget::MacOnly(
                "02:00:00:00:00:01".parse().unwrap(),
            ),
            post_lock_verification_retry_count: 0,
            boot_config_state,
        };
        for (name, state, eligible) in [
            (
                "failed host",
                ManagedHostState::Failed {
                    details: FailureDetails {
                        cause: FailureCause::UnhandledState {
                            err: "test failure".into(),
                        },
                        failed_at: Utc::now(),
                        source: FailureSource::StateMachine,
                    },
                    machine_id: machine_id.into(),
                    retry_count: 0,
                },
                true,
            ),
            (
                "boot preparation",
                boot(ReadyBootConfigState::Prepare),
                true,
            ),
            (
                "boot failure after recovery",
                boot(ReadyBootConfigState::Failed {
                    failure: "test failure".into(),
                }),
                true,
            ),
            (
                "boot writes in progress",
                boot(ReadyBootConfigState::CheckHostConfig),
                false,
            ),
            (
                "initialization in progress",
                ManagedHostState::HostInit {
                    machine_state: MachineState::EnableIpmiOverLan,
                },
                false,
            ),
            (
                "firmware maintenance in progress",
                ManagedHostState::HostReprovision {
                    reprovision_state: HostReprovisionState::WaitingForRackFirmwareUpgrade,
                    retry_count: 0,
                },
                false,
            ),
        ] {
            let mut txn = pool.begin().await?;
            super::create(
                txn.as_mut(),
                None,
                &machine_id.into(),
                state.clone(),
                None,
                super::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            assert_eq!(
                super::try_set_machine_maintenance_requested(
                    txn.as_mut(),
                    machine_id,
                    "rack-test",
                    super::MachineMaintenanceOperation::Reboot
                )
                .await?,
                eligible,
                "{name}"
            );
            let stored = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            assert_eq!(stored.state.value, state, "{name}");
            assert_eq!(
                stored.machine_maintenance_requested.is_some(),
                eligible,
                "{name}"
            );
            if eligible {
                assert!(
                    super::clear_unclaimed_machine_restart_request(
                        txn.as_mut(),
                        &machine_id,
                        "rack-test"
                    )
                    .await?,
                    "{name}"
                );
                let cancelled = super::find_one(txn.as_mut(), &machine_id, Default::default())
                    .await?
                    .unwrap();
                assert!(cancelled.machine_maintenance_requested.is_none(), "{name}");
                assert_eq!(cancelled.state.value, stored.state.value, "{name}");
                assert_eq!(cancelled.state.version, stored.state.version, "{name}");
            }
            txn.rollback().await?;
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_reset_cancels_unclaimed_request_after_another_workflow_starts(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id.into(),
            ManagedHostState::Ready,
            None,
            super::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        assert!(
            super::try_set_machine_maintenance_requested(
                txn.as_mut(),
                machine_id,
                "rack-test",
                super::MachineMaintenanceOperation::Reset { graceful: false }
            )
            .await?
        );
        // A controller snapshot from before admission can already be advancing
        // another workflow. It has not claimed this Reset by entering Maintenance.
        let active = ManagedHostState::HostReprovision {
            reprovision_state: model::machine::HostReprovisionState::WaitingForRackFirmwareUpgrade,
            retry_count: 0,
        };
        super::update_state(txn.as_mut(), &machine_id, &active).await?;
        assert!(
            super::clear_unclaimed_machine_restart_request(txn.as_mut(), &machine_id, "rack-test")
                .await?
        );
        let stored = super::find_one(txn.as_mut(), &machine_id, Default::default())
            .await?
            .unwrap();
        assert_eq!(stored.state.value, active);
        assert!(stored.machine_maintenance_requested.is_none());
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_reset_cancellation_preserves_claimed_and_foreign_requests(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use super::MachineMaintenanceOperation::{PowerOn, Reboot, Reset};
        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        for (initiator, operation, started, should_clear) in [
            ("rack-test", Reboot, false, true),
            ("operator", Reboot, false, false),
            ("rack-test", Reboot, true, false),
            ("rack-test", Reset { graceful: false }, false, true),
            ("rack-test", Reset { graceful: true }, false, true),
            ("operator", Reset { graceful: false }, false, false),
            ("rack-test", PowerOn, false, false),
            ("rack-test", Reset { graceful: false }, true, false),
        ] {
            let mut txn = pool.begin().await?;
            super::create(
                txn.as_mut(),
                None,
                &machine_id.into(),
                ManagedHostState::Ready,
                None,
                super::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            if matches!(operation, Reset { graceful: false }) {
                // Seed the predecessor JSON directly, independent of current serde behavior.
                sqlx::query("UPDATE machines SET machine_maintenance_requested = $1 WHERE id = $2")
                    .bind(serde_json::json!({
                        "requested_at": "2026-10-01T00:00:00Z",
                        "initiator": initiator,
                        "operation": { "operation": "reset" }
                    }))
                    .bind(machine_id)
                    .execute(txn.as_mut())
                    .await?;
            } else {
                super::set_machine_maintenance_requested(
                    txn.as_mut(),
                    machine_id,
                    initiator,
                    operation,
                )
                .await?;
            }
            let machine = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            let request = machine.machine_maintenance_requested.unwrap();
            let claimed_state = ManagedHostState::maintenance_for_request(request.clone());
            if started {
                assert_eq!(
                    super::try_update_controller_state(
                        txn.as_mut(),
                        &machine_id,
                        machine.state.version,
                        machine.state.version.increment(),
                        &claimed_state
                    )
                    .await?,
                    crate::ConditionalWrite::Applied(())
                );
            }
            assert_eq!(
                super::clear_unclaimed_machine_restart_request(
                    txn.as_mut(),
                    &machine_id,
                    "rack-test"
                )
                .await?,
                should_clear
            );
            let after = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            assert_eq!(
                after.machine_maintenance_requested,
                (!should_clear).then_some(request)
            );
            txn.rollback().await?;
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn reboot_verification_waits_for_a_concurrent_request(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use super::{MachineLastRebootRequested, MachineLastRebootRequestedMode};

        let machine_id: MachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        let captured = MachineLastRebootRequested {
            time: chrono::DateTime::from_timestamp(1_722_000_000, 0).unwrap(),
            mode: MachineLastRebootRequestedMode::Reboot,
            restart_verified: Some(false),
            verification_attempts: Some(0),
        };
        let mut setup = pool.begin().await?;
        super::create(
            setup.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            super::CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        super::record_reboot_request(&machine_id, setup.as_mut(), &captured).await?;
        setup.commit().await?;

        let newer = MachineLastRebootRequested {
            time: captured.time + chrono::Duration::seconds(1),
            ..captured
        };
        let mut replacement = pool.begin().await?;
        super::record_reboot_request(&machine_id, replacement.as_mut(), &newer).await?;

        let verify = async {
            let mut txn = pool.begin().await?;
            let result = super::update_restart_verification_status(
                &machine_id,
                captured,
                Some(true),
                1,
                &mut txn,
            )
            .await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        let commit_replacement = async {
            // The new request is still uncommitted. Verification must wait at
            // its locking read, then compare against the newly committed value.
            // Without that lock, it reads the old value and only its UPDATE waits.
            wait_until_blocked_on(&pool, "last_reboot_requested", 1).await;
            replacement.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(verify, commit_replacement)
        })
        .await??;
        assert_eq!(
            result,
            crate::ConditionalWrite::NotApplied(super::RebootVerificationNotCurrent)
        );
        let stored: sqlx::types::Json<MachineLastRebootRequested> =
            sqlx::query_scalar("SELECT last_reboot_requested FROM machines WHERE id=$1")
                .bind(machine_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored.0, newer);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn maintenance_admission_checks_the_captured_request(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use super::MachineMaintenanceOperation::{PowerOn, Reset};

        #[derive(Clone, Copy, Debug)]
        enum Pending {
            Current,
            Replaced,
            Cancelled,
            Legacy,
        }

        let machine_id: HostMachineId =
            "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30".parse()?;
        for (case, applied) in [
            (Pending::Current, true),
            (Pending::Replaced, false),
            (Pending::Cancelled, false),
            (Pending::Legacy, true),
        ] {
            let mut txn = pool.begin().await?;
            super::create(
                txn.as_mut(),
                None,
                &machine_id.into(),
                ManagedHostState::Ready,
                None,
                super::CURRENT_STATE_MODEL_VERSION,
            )
            .await?;
            // These requests were persisted before Reset carried a graceful flag.
            sqlx::query("UPDATE machines SET machine_maintenance_requested = $1 WHERE id = $2")
                .bind(serde_json::json!({
                    "requested_at": "2026-10-01T00:00:00Z",
                    "initiator": "component-manager",
                    "operation": { "operation": "reset" }
                }))
                .bind(machine_id)
                .execute(txn.as_mut())
                .await?;
            let snapshot = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            let captured = snapshot.machine_maintenance_requested.clone().unwrap();
            let next = if matches!(case, Pending::Legacy) {
                ManagedHostState::Maintenance {
                    operation: Reset { graceful: false },
                    request: None,
                }
            } else {
                ManagedHostState::maintenance_for_request(captured.clone())
            };
            match case {
                Pending::Current => {}
                Pending::Replaced => {
                    super::set_machine_maintenance_requested(
                        txn.as_mut(),
                        machine_id,
                        "component-manager",
                        PowerOn,
                    )
                    .await?
                }
                Pending::Cancelled | Pending::Legacy => assert_eq!(
                    super::clear_machine_maintenance_requested(txn.as_mut(), machine_id, &captured)
                        .await?,
                    crate::ConditionalWrite::Applied(())
                ),
            }
            let before = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            let history_before: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM machine_state_history WHERE object_id = $1",
            )
            .bind(machine_id.to_string())
            .fetch_one(txn.as_mut())
            .await?;
            let next_version = snapshot.state.version.increment();
            let result = super::try_update_controller_state(
                txn.as_mut(),
                &machine_id,
                snapshot.state.version,
                next_version,
                &next,
            )
            .await?;
            assert_eq!(
                result,
                if applied {
                    crate::ConditionalWrite::Applied(())
                } else {
                    crate::ConditionalWrite::NotApplied(super::ControllerStateNotCurrent)
                },
                "{case:?}"
            );
            let after = super::find_one(txn.as_mut(), &machine_id, Default::default())
                .await?
                .unwrap();
            assert_eq!(
                after.machine_maintenance_requested, before.machine_maintenance_requested,
                "{case:?}"
            );
            assert_eq!(
                after.state.value,
                if applied { next } else { before.state.value },
                "{case:?}"
            );
            assert_eq!(
                after.state.version,
                if applied {
                    next_version
                } else {
                    before.state.version
                },
                "{case:?}"
            );
            let history_after: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM machine_state_history WHERE object_id = $1",
            )
            .bind(machine_id.to_string())
            .fetch_one(txn.as_mut())
            .await?;
            assert_eq!(
                history_after,
                history_before + if applied { 1 } else { 0 },
                "{case:?}"
            );
            if matches!(case, Pending::Current) {
                assert_eq!(
                    super::clear_machine_maintenance_requested(txn.as_mut(), machine_id, &captured)
                        .await?,
                    crate::ConditionalWrite::Applied(())
                );
                let completed = super::find_one(txn.as_mut(), &machine_id, Default::default())
                    .await?
                    .unwrap();
                assert!(completed.machine_maintenance_requested.is_none());
            }
            txn.rollback().await?;
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn controller_state_persistence_preserves_advance_lock_order(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        let host_id = HostMachineId::try_from(machine_id)?;
        let mut setup_txn = pool.begin().await?;
        super::create(
            setup_txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        setup_txn.commit().await?;

        let mut legacy_txn = pool.begin().await?;
        let machine = super::find_one(
            legacy_txn.as_mut(),
            &host_id,
            MachineSearchConfig::default(),
        )
        .await?
        .expect("fixture host");
        let expected_version = machine.state.version;
        let new_version = expected_version.increment();
        let legacy_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(legacy_txn.as_mut())
            .await?;

        // Pause the legacy writer at the retention lock taken by `advance`.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(\
                hashtextextended($1, 'machine_state_history'::regclass::bigint)\
            )",
        )
        .bind(host_id.to_string())
        .execute(legacy_txn.as_mut())
        .await?;

        let mut controller_txn = pool.begin().await?;
        let controller_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(controller_txn.as_mut())
            .await?;
        let controller_write = async {
            let applied = super::try_update_controller_state(
                controller_txn.as_mut(),
                &host_id,
                expected_version,
                new_version,
                &ManagedHostState::Ready,
            )
            .await?;
            controller_txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(applied)
        };
        let legacy_write = async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (
                        SELECT 1 FROM pg_locks
                        WHERE pid = $1 AND locktype = 'advisory' AND NOT granted
                          AND $2 = ANY(pg_blocking_pids(pid))
                    )",
                )
                .bind(controller_pid)
                .bind(legacy_pid)
                .fetch_one(&pool)
                .await?;
                if waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }

            // Waiting for history must not hold the host row. NOWAIT catches
            // the reversed order without relying on a deadlock victim.
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(host_id)
                .fetch_one(legacy_txn.as_mut())
                .await?;
            super::advance(
                &machine,
                legacy_txn.as_mut(),
                &ManagedHostState::ForceDeletion,
                Some(new_version),
            )
            .await?;
            legacy_txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let (applied, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::try_join!(controller_write, legacy_write)
        })
        .await??;
        assert_eq!(
            applied,
            crate::ConditionalWrite::NotApplied(crate::ControllerStateNotCurrent),
            "the controller must reject the earlier snapshot"
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn dpu_group_update_fails_fast_while_the_host_writer_waits_for_its_target(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::test_support::postgres::wait_for_blocked_query;

        let (host_id, dpu_id, _) = nic_firmware_failure_fixture(&pool, 94).await?;
        let host_machine_id = MachineId::from(host_id);
        let dpu_machine_id = MachineId::from(dpu_id);
        let mut host_config = super::get_network_config(&pool, &host_machine_id).await?;
        let dpu_before = super::get_network_config(&pool, &dpu_machine_id).await?;
        host_config.value.use_admin_network = Some(true);
        let mut dpu_config = dpu_before.value.clone();
        dpu_config.loopback_ip = Some("192.0.2.42".parse()?);

        // The target DPU is held before the host starts its real group write.
        // Observing the group wait proves the host has already been written;
        // a DPU group update would complete the cycle if it waited for that host.
        let mut dpu_txn = pool.begin().await?;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
            .bind(dpu_id)
            .fetch_one(dpu_txn.as_mut())
            .await?;
        let dpu_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(dpu_txn.as_mut())
            .await?;
        let host = async {
            let mut txn = pool.begin().await?;
            let result = super::try_update_network_config(
                txn.as_mut(),
                &host_machine_id,
                host_config.version,
                &host_config.value,
            )
            .await?;
            txn.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(result)
        };
        tokio::pin!(host);
        tokio::select! {
            result = &mut host => panic!("host group bypassed the DPU gate: {result:?}"),
            _ = wait_for_blocked_query(&pool, dpu_pid, "WITH locked_group") => {}
        }
        let rejected = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            super::try_update_network_config(
                dpu_txn.as_mut(),
                &dpu_machine_id,
                dpu_before.version,
                &dpu_config,
            ),
        )
        .await;
        // Rollback includes the DPU content/version write preceding NOWAIT.
        dpu_txn.rollback().await?;
        let committed =
            tokio::time::timeout(std::time::Duration::from_secs(30), &mut host).await??;
        assert_eq!(committed, crate::ConditionalWrite::Applied(()));
        let error =
            rejected?.expect_err("the DPU group update must fail fast while the host writer waits");
        assert!(error.is_lock_not_available(), "{error:?}");
        let host_after = super::get_network_config(&pool, &host_machine_id).await?;
        let dpu_after = super::get_network_config(&pool, &dpu_machine_id).await?;
        assert_eq!(host_after.value, host_config.value);
        assert_eq!(
            host_after.version.version_nr(),
            host_config.version.version_nr() + 1
        );
        assert_eq!(dpu_after.value, dpu_before.value);
        assert_eq!(dpu_after.version, host_after.version);

        // A fresh DPU attempt uses the committed version and still preserves
        // the host's content while advancing the entire group's version.
        let mut retry = pool.begin().await?;
        assert_eq!(
            super::try_update_network_config(
                retry.as_mut(),
                &dpu_machine_id,
                dpu_after.version,
                &dpu_config,
            )
            .await?,
            crate::ConditionalWrite::Applied(())
        );
        retry.commit().await?;
        let host_final = super::get_network_config(&pool, &host_machine_id).await?;
        let dpu_final = super::get_network_config(&pool, &dpu_machine_id).await?;
        assert_eq!(host_final.value, host_after.value);
        assert_eq!(dpu_final.value, dpu_config);
        assert_eq!(host_final.version, dpu_final.version);
        assert_eq!(
            dpu_final.version.version_nr(),
            dpu_after.version.version_nr() + 1
        );
        Ok(())
    }

    /// Group persistence and DPU reprovisioning Mode::Set share DPU lock order.
    #[crate::sqlx_test]
    async fn network_group_locks_follow_the_independent_set_order(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use futures_util::future::maybe_done;

        use crate::test_support::postgres::wait_for_blocked_query;

        for (scenario, set_first, seed) in [
            ("Set queued first", true, 51),
            ("group queued first", false, 61),
        ] {
            let host_id = HostMachineId::try_from(machine_id(seed, MachineType::Host))?;
            let mut dpu_ids = [
                DpuMachineId::try_from(machine_id(seed, MachineType::Dpu))?,
                DpuMachineId::try_from(machine_id(seed + 1, MachineType::Dpu))?,
            ];
            dpu_ids.sort_unstable();
            let [low_id, high_id] = dpu_ids;
            let mut setup = pool.begin().await?;
            // Heap order opposes native ID order. Both live writers still
            // need to reach the lower DPU first.
            for id in [MachineId::from(host_id), high_id.into(), low_id.into()] {
                super::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
            }
            let segment_id: NetworkSegmentId = sqlx::query_scalar(
                "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
            )
            .bind(format!("group-order-{seed}"))
            .fetch_one(setup.as_mut())
            .await?;
            for (index, id) in dpu_ids.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO machine_interfaces
                        (machine_id, attached_dpu_machine_id, association_type, segment_id,
                         mac_address, primary_interface, hostname, interface_type)
                     VALUES ($1, $2, 'Machine', $3, $4::macaddr, false, $5, 'Data')",
                )
                .bind(host_id)
                .bind(id)
                .bind(segment_id)
                .bind(format!("02:00:00:{seed:02x}:00:{index:02x}"))
                .bind(format!("group-order-{seed}-{index}"))
                .execute(setup.as_mut())
                .await?;
            }
            let mut host_config =
                super::get_network_config(setup.as_mut(), &host_id.into()).await?;
            let dpu_configs = [
                super::get_network_config(setup.as_mut(), &low_id.into()).await?,
                super::get_network_config(setup.as_mut(), &high_id.into()).await?,
            ];
            setup.commit().await?;
            host_config.value.use_admin_network = Some(true);

            let mut gate = pool.begin().await?;
            super::lock_by_id_for_update(gate.as_mut(), &low_id.into()).await?;
            let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(gate.as_mut())
                .await?;
            let mut group_txn = pool.begin().await?;
            let group_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(group_txn.as_mut())
                .await?;
            let mut set_txn = pool.begin().await?;
            let set_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                .fetch_one(set_txn.as_mut())
                .await?;
            let group = maybe_done(async {
                sqlx::raw_sql(
                    "SET LOCAL enable_indexscan = off;
                     SET LOCAL enable_indexonlyscan = off;
                     SET LOCAL enable_bitmapscan = off",
                )
                .execute(group_txn.as_mut())
                .await?;
                let result = super::try_update_network_config(
                    group_txn.as_mut(),
                    &host_id.into(),
                    host_config.version,
                    &host_config.value,
                )
                .await;
                let finish = if result.is_ok() {
                    group_txn.commit().await
                } else {
                    group_txn.rollback().await
                };
                Ok::<_, Box<dyn std::error::Error>>((result, finish))
            });
            let set = maybe_done(async {
                sqlx::raw_sql(
                    "SET LOCAL enable_indexscan = off;
                     SET LOCAL enable_indexonlyscan = off;
                     SET LOCAL enable_bitmapscan = off",
                )
                .execute(set_txn.as_mut())
                .await?;
                // These are lock_attached_dpus and Mode::Set's database primitives.
                let locked = super::find(
                    set_txn.as_mut(),
                    crate::ObjectFilter::List(&dpu_ids),
                    MachineSearchConfig {
                        for_update: true,
                        ..Default::default()
                    },
                )
                .await?;
                if locked.len() != dpu_ids.len() {
                    return Err("Set did not lock the complete requested DPU set".into());
                }
                let mut requests = HashMap::new();
                for id in dpu_ids {
                    super::trigger_dpu_reprovisioning_request(
                        &id,
                        set_txn.as_mut(),
                        "ordered-set",
                        true,
                    )
                    .await?;
                    let machine = super::find_one(set_txn.as_mut(), &id, Default::default())
                        .await?
                        .expect("Set's locked DPU");
                    requests.insert(id, machine.reprovision_requested.expect("Set request"));
                }
                set_txn.commit().await?;
                Ok::<_, Box<dyn std::error::Error>>(requests)
            });
            tokio::pin!(group, set);
            let readiness = tokio::time::timeout(std::time::Duration::from_secs(25), async {
                if set_first {
                    tokio::select! {
                        () = &mut set => return Err("Set passed the lower DPU gate"),
                        _ = wait_for_blocked_query(&pool, gate_pid, "SELECT row_to_json") => {}
                    }
                    tokio::select! {
                        () = &mut set => return Err("Set passed the lower DPU gate"),
                        () = &mut group => return Err("group did not wait behind Set"),
                        _ = wait_for_blocked_query(&pool, set_pid, "WITH locked_group") => {}
                    }
                } else {
                    tokio::select! {
                        () = &mut group => return Err("group passed the lower DPU gate"),
                        _ = wait_for_blocked_query(&pool, gate_pid, "WITH locked_group") => {}
                    }
                    tokio::select! {
                        () = &mut group => return Err("group passed the lower DPU gate"),
                        () = &mut set => return Err("Set did not wait behind group"),
                        _ = wait_for_blocked_query(&pool, group_pid, "SELECT row_to_json") => {}
                    }
                }
                Ok::<(), &str>(())
            })
            .await;
            let high_probe =
                sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE NOWAIT")
                    .bind(high_id)
                    .fetch_one(&pool)
                    .await;
            let release = gate.commit().await;
            let finished = {
                let joined = async {
                    tokio::join!(&mut group, &mut set);
                };
                tokio::pin!(joined);
                let finished =
                    tokio::time::timeout(std::time::Duration::from_secs(30), &mut joined)
                        .await
                        .is_ok();
                if !finished {
                    for pid in [group_pid, set_pid] {
                        sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
                            .bind(pid)
                            .fetch_one(&pool)
                            .await?;
                    }
                    tokio::time::timeout(std::time::Duration::from_secs(30), &mut joined)
                        .await
                        .expect("cancelled group/Set writers must join");
                }
                finished
            };
            let group_result = group.as_mut().take_output().expect("group writer joined");
            let set_result = set.as_mut().take_output().expect("Set writer joined");
            tracing::info!(
                scenario,
                gate_pid,
                group_pid,
                set_pid,
                ?readiness,
                ?high_probe,
                ?group_result,
                ?set_result,
                "Joined the actual network-group and Set primitives"
            );
            release?;
            readiness
                .expect("both actual row waits must appear")
                .expect("both writers reached their ordered gates");
            assert!(finished, "{scenario}: writers required cancellation");
            high_probe.expect("neither writer may retain the higher DPU before the lower one");
            let (group_result, finish) = group_result?;
            finish?;
            assert_eq!(group_result?, crate::ConditionalWrite::Applied(()));
            let requests = set_result?;
            let mut check = pool.begin().await?;
            let host_after = super::get_network_config(check.as_mut(), &host_id.into()).await?;
            assert_eq!(host_after.value, host_config.value);
            assert_eq!(
                host_after.version.version_nr(),
                host_config.version.version_nr() + 1
            );
            for (index, id) in dpu_ids.iter().enumerate() {
                let machine = super::find_one(check.as_mut(), id, Default::default())
                    .await?
                    .unwrap();
                assert_eq!(machine.network_config.version, host_after.version);
                assert_eq!(machine.network_config.value, dpu_configs[index].value);
                assert_eq!(machine.reprovision_requested.as_ref(), Some(&requests[id]));
            }
            check.commit().await?;
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn completion_markers_follow_state_committed_after_transaction_start(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Clone, Copy)]
        enum CompletionMarker {
            Cleanup,
            Discovery,
            MachineValidation,
            Reboot,
        }

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        let mut setup_txn = pool.begin().await?;
        super::create(
            setup_txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        setup_txn.commit().await?;

        for (case, marker) in [
            ("cleanup", CompletionMarker::Cleanup),
            ("discovery", CompletionMarker::Discovery),
            ("machine validation", CompletionMarker::MachineValidation),
            ("reboot", CompletionMarker::Reboot),
        ] {
            let mut marker_txn = pool.begin().await?;
            let marker_transaction_started_at: chrono::DateTime<chrono::Utc> =
                sqlx::query_scalar("SELECT transaction_timestamp()")
                    .fetch_one(marker_txn.as_mut())
                    .await?;
            let machine = super::find_one(
                marker_txn.as_mut(),
                &machine_id,
                MachineSearchConfig::default(),
            )
            .await?
            .unwrap_or_else(|| panic!("{case}: machine should exist before recording completion"));

            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            let mut state_txn = pool.begin().await?;
            let machine_for_state_update = super::find_one(
                state_txn.as_mut(),
                &machine_id,
                MachineSearchConfig::default(),
            )
            .await?
            .unwrap_or_else(|| {
                panic!("{case}: machine should exist before advancing its state version")
            });
            super::advance(
                &machine_for_state_update,
                state_txn.as_mut(),
                &ManagedHostState::Ready,
                None,
            )
            .await?;
            state_txn.commit().await?;

            match marker {
                CompletionMarker::Cleanup => {
                    super::update_cleanup_time(&machine, marker_txn.as_mut()).await?
                }
                CompletionMarker::Discovery => {
                    super::update_discovery_time(&machine.id, marker_txn.as_mut()).await?
                }
                CompletionMarker::MachineValidation => {
                    super::update_machine_validation_time(&machine.id, marker_txn.as_mut()).await?
                }
                CompletionMarker::Reboot => {
                    super::update_reboot_time(&machine, marker_txn.as_mut()).await?
                }
            }
            marker_txn.commit().await?;

            let mut verify_txn = pool.begin().await?;
            let machine = super::find_one(
                verify_txn.as_mut(),
                &machine_id,
                MachineSearchConfig::default(),
            )
            .await?
            .unwrap_or_else(|| panic!("{case}: machine should exist after recording completion"));
            let marker_time = match marker {
                CompletionMarker::Cleanup => machine.status.last_cleanup_time,
                CompletionMarker::Discovery => machine.status.last_discovery_time,
                CompletionMarker::MachineValidation => machine.last_machine_validation_time,
                CompletionMarker::Reboot => machine.status.last_reboot_time,
            }
            .unwrap_or_else(|| panic!("{case}: completion time should be recorded"));
            assert!(
                marker_transaction_started_at < machine.state.version.timestamp(),
                "{case}: the marker transaction must start before advancing the state version"
            );
            assert!(
                marker_time > machine.state.version.timestamp(),
                "{case}: completion recorded after a state transition must be newer than that state version"
            );
            verify_txn.commit().await?;
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn dpu_creation_keeps_unavailable_fnn_asn_nonfatal(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        const RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC: &str =
            "carbide_resource_pool_lifecycle_failures_total";

        let common_pools = common_pools_without_seeded_values();
        let dpu_id =
            MachineId::from_str("fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng")?;
        for (scenario, exhaust_pool, expected_failure) in [
            ("exhausted", true, "exhausted"),
            ("unconfigured", false, "configuration"),
        ] {
            let mut txn = pool.begin().await?;
            let asn_pool = &common_pools.ethernet.pool_fnn_asn;
            if exhaust_pool {
                crate::resource_pool::populate(asn_pool, txn.as_mut(), vec![64512], true).await?;
                crate::resource_pool::allocate(
                    asn_pool,
                    txn.as_mut(),
                    model::resource_pool::OwnerType::Machine,
                    "existing-dpu",
                    None,
                )
                .await?;
            }
            let stats = crate::resource_pool::stats(txn.as_mut(), asn_pool.name()).await?;
            assert_eq!(
                (stats.auto_assign_free, stats.auto_assign_used),
                (0, usize::from(exhaust_pool)),
                "{scenario}: verify the pool state before creating the DPU"
            );
            let metrics = MetricsCapture::start();

            let (machine, logs) = capture_logs_async(super::create(
                txn.as_mut(),
                Some(&common_pools),
                &dpu_id,
                ManagedHostState::Ready,
                None,
                2,
            ))
            .await;
            let machine = machine?;

            assert_eq!(machine.asn, None, "{scenario}");
            let event_logs = logs
                .iter()
                .filter(|log| log.metadata_name == "dpu_asn_allocation_failed")
                .collect::<Vec<_>>();
            assert_eq!(event_logs.len(), 1, "{scenario}");
            let log = event_logs[0];
            assert_eq!(log.level, tracing::Level::INFO, "{scenario}");
            assert_eq!(log.message, "Failed to allocate asn for dpu", "{scenario}");
            assert_eq!(
                log.field("stable_machine_id"),
                Some(dpu_id.to_string().as_str()),
                "{scenario}"
            );
            assert_eq!(log.field("failure"), Some(expected_failure), "{scenario}");
            assert_eq!(
                log.field("failure_policy"),
                Some("best_effort"),
                "{scenario}"
            );
            assert_eq!(
                log.field("allocation_mode"),
                Some("automatic"),
                "{scenario}"
            );
            assert_eq!(
                metrics.counter_delta(
                    RESOURCE_POOL_LIFECYCLE_FAILURES_METRIC,
                    &[
                        ("operation", "allocate"),
                        ("failure", expected_failure),
                        ("failure_policy", "best_effort"),
                        ("allocation_mode", "automatic"),
                        ("value_type", "integer"),
                    ],
                ),
                1.0,
                "{scenario}"
            );
            txn.rollback().await?;
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn concurrent_asn_backfills_leave_one_reservation(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, None).await?;
        let dpu_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &dpu_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        txn.commit().await?;

        let mut machine_lock = pool.begin().await?;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
            .bind(dpu_id)
            .fetch_one(machine_lock.as_mut())
            .await?;

        let release_machine = async {
            // Both backfills must reserve an ASN and reach the machine update
            // before either can assign it. The losing reservation must be freed.
            wait_until_blocked_on(&pool, "UPDATE machines set asn=", 2).await;
            machine_lock.commit().await
        };
        let (first, second, released) =
            tokio::time::timeout(std::time::Duration::from_secs(90), async {
                tokio::join!(
                    super::update_dpu_asns(&pool, &common_pools),
                    super::update_dpu_asns(&pool, &common_pools),
                    release_machine,
                )
            })
            .await
            .expect("both ASN backfills must finish after the machine lock is released");
        first?;
        second?;
        released?;

        let assigned: i64 = sqlx::query_scalar("SELECT asn FROM machines WHERE id = $1")
            .bind(dpu_id)
            .fetch_one(&pool)
            .await?;
        let reserved: Vec<i64> = sqlx::query_scalar(
            "SELECT value::bigint FROM resource_pool WHERE name = $1 AND state = $2",
        )
        .bind(FNN_ASN)
        .bind(sqlx::types::Json(
            model::resource_pool::ResourcePoolEntryState::Allocated {
                owner: dpu_id.to_string(),
                owner_type: model::resource_pool::OwnerType::Machine.to_string(),
            },
        ))
        .fetch_all(&pool)
        .await?;
        assert_eq!(reserved, vec![assigned]);
        Ok(())
    }

    fn integer_pool() -> ResourcePoolDef {
        ResourcePoolDef {
            ranges: vec![Range {
                start: "1".to_string(),
                end: "16".to_string(),
                auto_assign: true,
            }],
            prefix: None,
            pool_type: ResourcePoolType::Integer,
            delegate_prefix_len: None,
        }
    }

    fn address_pool(pool_type: ResourcePoolType, prefix: &str) -> ResourcePoolDef {
        ResourcePoolDef {
            ranges: Vec::new(),
            prefix: Some(prefix.to_string()),
            pool_type,
            delegate_prefix_len: None,
        }
    }

    async fn common_pools(
        pool: &sqlx::PgPool,
        loopback_ip_v6_prefix: Option<&str>,
    ) -> Result<Arc<CommonPools>, Box<dyn std::error::Error>> {
        let mut definitions = HashMap::from([
            (
                LOOPBACK_IP.to_string(),
                address_pool(ResourcePoolType::Ipv4, "192.0.2.0/29"),
            ),
            (VLANID.to_string(), integer_pool()),
            (VNI.to_string(), integer_pool()),
            (VPC_VNI.to_string(), integer_pool()),
            (FNN_ASN.to_string(), integer_pool()),
        ]);
        if let Some(prefix) = loopback_ip_v6_prefix {
            definitions.insert(
                LOOPBACK_IP_V6.to_string(),
                address_pool(ResourcePoolType::Ipv6, prefix),
            );
        }

        let mut txn = pool.begin().await?;
        crate::resource_pool::define_all_from(txn.as_mut(), &definitions).await?;
        txn.commit().await?;

        Ok(crate::resource_pool::create_common_pools(pool.clone(), HashSet::new()).await?)
    }

    async fn wait_until_blocked_on(pool: &sqlx::PgPool, relation: &str, expected_count: i64) {
        for _ in 0..600 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND wait_event_type = 'Lock'
                   AND query ILIKE '%' || $1 || '%'",
            )
            .bind(relation)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= expected_count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("expected {expected_count} queries blocked on {relation}");
    }

    /// The machine snapshot reports the BMC IP from the *live* `machine_interface_addresses`
    /// row, and reports `None` once that address is gone -- it must never fall back to the
    /// stale copy cached in `machine_topologies.topology.bmc_info.ip`.
    ///
    /// Exercises the BMC subquery in `machine_snapshots.sql.template`, which now reads
    /// `host(bmc_addr.address)` directly instead of coalescing onto the topology copy.
    #[crate::sqlx_test]
    async fn test_snapshot_bmc_ip_follows_live_interface_not_stale_topology(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;

        let live_bmc_ip: IpAddr = "10.1.2.3".parse()?;
        // A different value in the topology copy makes a fallback observable: if the
        // snapshot ever read the stale copy we'd see this instead of the live address.
        let stale_topology_ip: IpAddr = "10.9.9.9".parse()?;

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        super::create(
            txn.as_mut(),
            None,
            &machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;

        // A BMC interface attached to the machine, plus its live address (a DHCP lease).
        let segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version) VALUES ($1, 'V1-T0') RETURNING id",
        )
        .bind("bmc-live-vs-stale")
        .fetch_one(txn.as_mut())
        .await?;
        let bmc_interface_id: MachineInterfaceId = sqlx::query_scalar(
            "INSERT INTO machine_interfaces
                 (machine_id, association_type, segment_id, mac_address,
                  primary_interface, hostname, interface_type)
             VALUES ($1, 'Machine', $2, $3::macaddr, false, 'bmc-live-vs-stale', 'Bmc')
             RETURNING id",
        )
        .bind(machine_id)
        .bind(segment_id)
        .bind("02:00:00:00:00:01")
        .fetch_one(txn.as_mut())
        .await?;
        crate::machine_interface_address::insert(
            txn.as_mut(),
            bmc_interface_id,
            live_bmc_ip,
            AllocationType::Dhcp,
        )
        .await?;

        // A topology row that *also* carries a (stale) BMC IP -- the removed fallback.
        let topology = TopologyData {
            discovery_data: DiscoveryData {
                info: HardwareInfo::default(),
            },
            bmc_info: BmcInfo {
                ip: Some(stale_topology_ip),
                ..Default::default()
            },
        };
        sqlx::query("INSERT INTO machine_topologies (machine_id, topology) VALUES ($1, $2::jsonb)")
            .bind(machine_id)
            .bind(sqlx::types::Json(&topology))
            .execute(txn.as_mut())
            .await?;

        // With a live address present, the snapshot reports it.
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.status.bmc_info.ip, Some(live_bmc_ip));

        // Release the lease: drop the live address but keep the interface and the topology
        // row (with its stale bmc_info.ip) intact.
        crate::machine_interface_address::delete(txn.as_mut(), &bmc_interface_id).await?;

        // The key assertion: with no live address, the snapshot reports None rather than
        // falling back to the stale topology copy.
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.status.bmc_info.ip, None);

        // Now drop the BMC interface row entirely; the topology row (with its stale
        // bmc_info.ip) stays. Even with no live BMC interface at all, the snapshot must
        // still report None -- it must not fall back to the stale copy via the outer COALESCE.
        sqlx::query("DELETE FROM machine_interfaces WHERE id = $1")
            .bind(bmc_interface_id)
            .execute(txn.as_mut())
            .await?;
        let machine = super::find_one(txn.as_mut(), &machine_id, MachineSearchConfig::default())
            .await?
            .expect("machine snapshot should load");
        assert_eq!(machine.status.bmc_info.ip, None);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]

    async fn test_set_firmware_autoupdate(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();
        let id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        super::create(&mut txn, None, &id, ManagedHostState::Ready, None, 2).await?;
        super::set_firmware_autoupdate(&mut txn, &id, Some(true)).await?;
        txn.commit().await?;
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();

        let host = crate::machine::find_one(&mut *txn, &id, MachineSearchConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(host.config.firmware_autoupdate, Some(true));

        txn.commit().await?;
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = pool.begin().await.unwrap();
        super::set_firmware_autoupdate(&mut txn, &id, None).await?;
        let host = crate::machine::find_one(&mut *txn, &id, MachineSearchConfig::default())
            .await
            .unwrap()
            .unwrap();
        assert!(host.config.firmware_autoupdate.is_none());
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_pool_absence_preserves_ipv4_only_creation(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, None).await?;
        let dpu_machine_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;

        let mut txn = pool.begin().await?;
        assert_eq!(
            super::allocate_loopback_ip_v6(
                &common_pools,
                txn.as_mut(),
                &dpu_machine_id.to_string(),
                None,
            )
            .await?,
            None
        );

        assert!(
            super::allocate_loopback_ip_v6(
                &common_pools,
                txn.as_mut(),
                &dpu_machine_id.to_string(),
                Some("2001:db8::20".parse()?)
            )
            .await
            .is_err()
        );

        let dpu = super::create(
            txn.as_mut(),
            Some(&common_pools),
            &dpu_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        assert_eq!(dpu.network_config.loopback_ip_v6, None);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn configured_ipv6_loopback_reservation_is_reused_and_exhausts_for_another_owner(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:1::1/128")).await?;
        let first_dpu_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;
        let second_dpu_id =
            MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?;

        let mut txn = pool.begin().await?;
        let first_dpu = super::create(
            txn.as_mut(),
            Some(&common_pools),
            &first_dpu_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        assert_eq!(
            first_dpu.network_config.loopback_ip_v6,
            Some("2001:db8:1::1".parse::<Ipv6Addr>()?)
        );
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let reused = super::allocate_loopback_ip_v6(
            &common_pools,
            txn.as_mut(),
            &first_dpu_id.to_string(),
            None,
        )
        .await?;
        assert_eq!(reused, first_dpu.network_config.loopback_ip_v6);
        let stats_after_reuse = crate::resource_pool::stats(txn.as_mut(), LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_reuse.used, 1);
        assert_eq!(stats_after_reuse.free, 0);
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let error = super::create(
            txn.as_mut(),
            Some(&common_pools),
            &second_dpu_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await
        .expect_err("a configured but exhausted IPv6 loopback pool must fail DPU creation");
        assert!(matches!(error, crate::DatabaseError::ResourceExhausted(_)));
        txn.rollback().await?;

        let stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats.used, 1);
        assert_eq!(stats.free, 0);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_survives_legacy_updates_and_reuses_its_reservation(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:4::1/128")).await?;
        let dpu_machine_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;

        let mut txn = pool.begin().await?;
        let dpu = super::create(
            txn.as_mut(),
            Some(&common_pools),
            &dpu_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        let allocated = dpu
            .network_config
            .loopback_ip_v6
            .expect("the configured pool should reserve an IPv6 loopback");
        txn.commit().await?;

        // An older API replaces the complete document without the field it
        // does not know. The trigger restores that field while retaining its
        // changes to fields it does know.
        let legacy_network_config = serde_json::json!({
            "loopback_ip": null,
            "use_admin_network": false,
            "quarantine_state": null,
            "use_admin_network_changed": null
        });
        let update_query = "UPDATE machines SET network_config = $1 WHERE id = $2";
        let mut txn = pool.begin().await?;
        sqlx::query(update_query)
            .bind(sqlx::types::Json(&legacy_network_config))
            .bind(dpu_machine_id)
            .execute(txn.as_mut())
            .await?;
        let after_legacy_update = super::get_network_config(txn.as_mut(), &dpu_machine_id).await?;
        assert_eq!(after_legacy_update.value.loopback_ip_v6, Some(allocated));
        assert_eq!(after_legacy_update.value.use_admin_network, Some(false));
        let stats_after_legacy_update =
            crate::resource_pool::stats(txn.as_mut(), LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_legacy_update.used, 1);
        assert_eq!(stats_after_legacy_update.free, 0);
        txn.commit().await?;

        let current_network_config = serde_json::json!({
            "loopback_ip": null,
            "loopback_ip_v6": null,
            "use_admin_network": false,
            "quarantine_state": null,
            "use_admin_network_changed": null
        });
        let mut txn = pool.begin().await?;
        sqlx::query(update_query)
            .bind(sqlx::types::Json(&current_network_config))
            .bind(dpu_machine_id)
            .execute(txn.as_mut())
            .await?;
        let after_explicit_clear = super::get_network_config(txn.as_mut(), &dpu_machine_id).await?;
        assert_eq!(after_explicit_clear.value.loopback_ip_v6, None);

        let reused = super::allocate_loopback_ip_v6(
            &common_pools,
            txn.as_mut(),
            &dpu_machine_id.to_string(),
            None,
        )
        .await?;
        assert_eq!(reused, Some(allocated));
        let stats_after_reuse = crate::resource_pool::stats(txn.as_mut(), LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_reuse.used, 1);
        assert_eq!(stats_after_reuse.free, 0);
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_reloads_each_dpu_and_is_idempotent(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:2::/125")).await?;
        let host_machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")?;
        let dpu_machine_ids = [
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?,
            MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?,
        ];

        // These rows model DPUs that predate `loopback_ip_v6`, so create them
        // without common pools and then connect both to the same host group.
        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &host_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        for dpu_machine_id in &dpu_machine_ids {
            super::create(
                txn.as_mut(),
                None,
                dpu_machine_id,
                ManagedHostState::Ready,
                None,
                2,
            )
            .await?;
        }

        let network_segment_id: NetworkSegmentId = sqlx::query_scalar(
            "INSERT INTO network_segments (name, version)
             VALUES ('ipv6-loopback-backfill', 'V1-T0') RETURNING id",
        )
        .fetch_one(txn.as_mut())
        .await?;
        for (index, dpu_machine_id) in dpu_machine_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type,
                     segment_id, mac_address, primary_interface, hostname)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, $5, $6)",
            )
            .bind(host_machine_id)
            .bind(dpu_machine_id)
            .bind(network_segment_id)
            .bind(format!("02:00:00:00:10:{index:02x}"))
            .bind(index == 0)
            .bind(format!("backfill-dpu-{index}"))
            .execute(txn.as_mut())
            .await?;
        }
        txn.commit().await?;

        super::update_dpu_loopback_ips_v6(&pool, &common_pools).await?;

        let mut txn = pool.begin().await?;
        let host_config = super::get_network_config(txn.as_mut(), &host_machine_id).await?;
        let first_dpu_config = super::get_network_config(txn.as_mut(), &dpu_machine_ids[0]).await?;
        let second_dpu_config =
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[1]).await?;
        let allocated = [
            first_dpu_config
                .value
                .loopback_ip_v6
                .expect("first DPU should be backfilled"),
            second_dpu_config
                .value
                .loopback_ip_v6
                .expect("second DPU should be backfilled"),
        ];
        assert_ne!(allocated[0], allocated[1]);
        assert_eq!(host_config.value.loopback_ip_v6, None);
        assert_eq!(host_config.version, first_dpu_config.version);
        assert_eq!(first_dpu_config.version, second_dpu_config.version);
        let version_after_backfill = host_config.version;
        txn.commit().await?;

        let stats_after_backfill = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_backfill.used, 2);

        // A second startup pass must preserve both reservations and avoid a
        // version bump: `None` is the only row state the backfill owns.
        super::update_dpu_loopback_ips_v6(&pool, &common_pools).await?;
        let stats_after_rerun = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_rerun, stats_after_backfill);

        let mut txn = pool.begin().await?;
        let host_config = super::get_network_config(txn.as_mut(), &host_machine_id).await?;
        let first_dpu_config = super::get_network_config(txn.as_mut(), &dpu_machine_ids[0]).await?;
        let second_dpu_config =
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[1]).await?;
        assert_eq!(host_config.version, version_after_backfill);
        assert_eq!(
            [
                first_dpu_config.value.loopback_ip_v6.unwrap(),
                second_dpu_config.value.loopback_ip_v6.unwrap(),
            ],
            allocated
        );
        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_skips_force_deleting_dpus(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:5::1/128")).await?;
        let dpu_ids = [
            machine_id(100, MachineType::Dpu),
            machine_id(101, MachineType::Dpu),
        ];
        let mut txn = pool.begin().await?;
        for id in dpu_ids {
            super::create(txn.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        }
        let ordered_dpus: Vec<MachineId> =
            sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
                .bind(dpu_ids.as_slice())
                .fetch_all(txn.as_mut())
                .await?;
        let deleting_id = ordered_dpus[0];
        let eligible_id = ordered_dpus[1];
        sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
            .bind(deleting_id)
            .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
            .execute(txn.as_mut())
            .await?;
        txn.commit().await?;
        let deleting_before = super::get_network_config(&pool, &deleting_id).await?;

        // Deletion is already committed. A separate owner keeps that row busy;
        // neither contention nor later pool exhaustion should override its skip.
        let mut gate = pool.begin().await?;
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
            .bind(deleting_id)
            .fetch_one(gate.as_mut())
            .await?;
        let passes = async {
            super::update_dpu_loopback_ips_v6(&pool, &common_pools).await?;
            let eligible_first = super::get_network_config(&pool, &eligible_id).await?;
            let first_stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;

            // The independent DPU consumed the only address. The remaining
            // candidate is still force-deleting and still locked by the gate.
            super::update_dpu_loopback_ips_v6(&pool, &common_pools).await?;
            let deleting_after = super::get_network_config(&pool, &deleting_id).await?;
            let eligible_second = super::get_network_config(&pool, &eligible_id).await?;
            let second_stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
            let deleting_allocations: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM resource_pool_allocations
                 WHERE pool_name = $1 AND owner_id = $2",
            )
            .bind(LOOPBACK_IP_V6)
            .bind(deleting_id.to_string())
            .fetch_one(&pool)
            .await?;
            Ok::<_, Box<dyn std::error::Error>>((
                eligible_first,
                first_stats,
                deleting_after,
                eligible_second,
                second_stats,
                deleting_allocations,
            ))
        };
        tokio::pin!(passes);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), &mut passes).await;
        let released = gate.rollback().await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // Join the real future after releasing its gate before failing.
                let late_result =
                    tokio::time::timeout(std::time::Duration::from_secs(30), &mut passes).await;
                return Err(format!(
                    "backfill exceeded its gated deadline: {error}; \
                     release={released:?}; late_result={late_result:?}"
                )
                .into());
            }
        };
        released?;
        let (
            eligible_first,
            first_stats,
            deleting_after,
            eligible_second,
            second_stats,
            deleting_allocations,
        ) = result?;
        assert_eq!(
            eligible_first.value.loopback_ip_v6,
            Some("2001:db8:5::1".parse()?)
        );
        assert_eq!(first_stats.used, 1);
        assert_eq!(first_stats.free, 0);
        assert_eq!(deleting_after.value, deleting_before.value);
        assert_eq!(deleting_after.version, deleting_before.version);
        assert_eq!(deleting_after.value.loopback_ip_v6, None);
        assert_eq!(deleting_allocations, 0);
        assert_eq!(eligible_second.value, eligible_first.value);
        assert_eq!(eligible_second.version, eligible_first.version);
        assert_eq!(second_stats, first_stats);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_releases_reservation_after_late_force_deletion(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use futures::future::maybe_done;

        use crate::test_support::postgres::wait_for_blocked_query;

        let common_pools = common_pools(&pool, Some("2001:db8:8::1/128")).await?;
        let dpu_ids = [
            machine_id(102, MachineType::Dpu),
            machine_id(103, MachineType::Dpu),
        ];
        let mut setup = pool.begin().await?;
        for id in dpu_ids {
            super::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
        }
        let ordered_dpus: Vec<MachineId> =
            sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
                .bind(dpu_ids.as_slice())
                .fetch_all(setup.as_mut())
                .await?;
        setup.commit().await?;
        let deleting_id = ordered_dpus[0];
        let later_id = ordered_dpus[1];
        let deleting_before = super::find_one(&pool, &deleting_id, Default::default())
            .await?
            .expect("the first Ready DPU exists");
        let later_before = super::get_network_config(&pool, &later_id).await?;
        let stats_before = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats_before.used, 0);
        assert_eq!(stats_before.free, 1);

        // Only a fresh allocation writes this table. Observing that statement
        // behind the gate proves the initial eligibility read has passed;
        // neither the gate nor the backfill has locked the DPU row yet.
        let mut gate = pool.begin().await?;
        sqlx::query("LOCK TABLE resource_pool_allocations IN SHARE MODE")
            .execute(gate.as_mut())
            .await?;
        let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(gate.as_mut())
            .await?;
        let backfill_pool = pool.clone();
        let backfill_common_pools = Arc::clone(&common_pools);
        let task = tokio::spawn(async move {
            super::update_dpu_loopback_ips_v6(&backfill_pool, &backfill_common_pools).await
        });
        let abort_backfill = task.abort_handle();
        let backfill = maybe_done(task);
        tokio::pin!(backfill);
        let at_gate = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::select! {
                () = &mut backfill => {
                    return Err("backfill finished before the fresh allocation gate".into());
                }
                _ = wait_for_blocked_query(&pool, gate_pid, "WITH allocate AS") => {}
            }
            let mut deletion = pool.begin().await?;
            let advanced = super::advance(
                &deleting_before,
                deletion.as_mut(),
                &ManagedHostState::ForceDeletion,
                None,
            )
            .await?;
            deletion.commit().await?;
            Ok::<_, Box<dyn std::error::Error>>(advanced)
        })
        .await;
        let released = gate.rollback().await;
        let joined_after_release =
            tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill)
                .await
                .is_ok();
        if !joined_after_release {
            abort_backfill.abort();
            tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill)
                .await
                .expect("cancelled backfill must join after releasing its allocation gate");
        }
        let result = backfill
            .as_mut()
            .take_output()
            .expect("backfill task joined");
        released?;
        assert!(
            at_gate.expect("ForceDeletion must commit while allocation is gated")?,
            "the state writer must advance the first DPU"
        );
        assert!(
            joined_after_release,
            "backfill required cancellation after gate release"
        );
        result??;

        let deleting_after = super::find_one(&pool, &deleting_id, Default::default())
            .await?
            .expect("ForceDeletion must leave the DPU available for its controller");
        assert_eq!(deleting_after.state.value, ManagedHostState::ForceDeletion);
        assert_eq!(
            deleting_after.network_config.value,
            deleting_before.network_config.value
        );
        assert_eq!(
            deleting_after.network_config.version,
            deleting_before.network_config.version
        );
        let deleting_allocations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource_pool_allocations
             WHERE pool_name = $1 AND owner_type = 'machine' AND owner_id = $2",
        )
        .bind(LOOPBACK_IP_V6)
        .bind(deleting_id.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(deleting_allocations, 0);

        // The next independent DPU can use the only address only if the
        // rejected reservation was rolled back before the scan continued.
        let later_after = super::get_network_config(&pool, &later_id).await?;
        let mut expected_later = later_before.value;
        expected_later.loopback_ip_v6 = Some("2001:db8:8::1".parse()?);
        assert_eq!(later_after.value, expected_later);
        assert_eq!(
            later_after.version.version_nr(),
            later_before.version.version_nr() + 1
        );
        let stats_after = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after.used, 1);
        assert_eq!(stats_after.free, 0);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_retries_after_network_config_conflict(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:6::1/128")).await?;
        let dpu_machine_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;

        let mut txn = pool.begin().await?;
        super::create(
            txn.as_mut(),
            None,
            &dpu_machine_id,
            ManagedHostState::Ready,
            None,
            2,
        )
        .await?;
        let reserved = super::allocate_loopback_ip_v6(
            &common_pools,
            txn.as_mut(),
            &dpu_machine_id.to_string(),
            None,
        )
        .await?
        .expect("configured pool should reserve an IPv6 loopback");
        txn.commit().await?;

        // Hold the existing reservation after the backfill reads its machine
        // version. This gives another writer a deterministic window to bump
        // that version before the first compare-and-swap reaches the row.
        let mut reservation_lock = pool.begin().await?;
        sqlx::query(
            "SELECT value FROM resource_pool
             WHERE name = $1 AND value = $2
             FOR UPDATE",
        )
        .bind(LOOPBACK_IP_V6)
        .bind(reserved.to_string())
        .fetch_one(reservation_lock.as_mut())
        .await?;

        let backfill_pool = pool.clone();
        let backfill_common_pools = Arc::clone(&common_pools);
        let backfill = tokio::spawn(async move {
            super::update_dpu_loopback_ips_v6(&backfill_pool, &backfill_common_pools).await
        });
        wait_until_blocked_on(&pool, "resource_pool", 1).await;

        let mut txn = pool.begin().await?;
        let (mut network_config, version) =
            super::get_network_config(txn.as_mut(), &dpu_machine_id)
                .await?
                .take();
        network_config.use_admin_network = Some(false);
        assert_eq!(
            super::try_update_network_config(
                txn.as_mut(),
                &dpu_machine_id,
                version,
                &network_config,
            )
            .await?,
            crate::ConditionalWrite::Applied(())
        );
        txn.commit().await?;
        reservation_lock.commit().await?;

        tokio::time::timeout(std::time::Duration::from_secs(10), backfill)
            .await??
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;

        let persisted = super::get_network_config(&pool, &dpu_machine_id).await?;
        network_config.loopback_ip_v6 = Some(reserved);
        assert_eq!(persisted.value, network_config);
        assert_eq!(persisted.version.version_nr(), version.version_nr() + 2);
        let stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats.used, 1);
        assert_eq!(stats.free, 0);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_rolls_back_busy_groups_and_retries_a_later_pass(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        const DROP_ATTEMPT_COUNTER: &str =
            "DROP TRIGGER count_busy_backfill_allocation ON resource_pool_allocations;
             DROP FUNCTION count_busy_backfill_allocation();
             DROP SEQUENCE busy_backfill_attempts";

        let common_pools = common_pools(&pool, Some("2001:db8:7::/126")).await?;
        for (scenario, seed, target_busy) in [("busy group", 0, false), ("busy target", 2, true)] {
            let pool_before = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
            let host_id = machine_id(seed, MachineType::Host);
            let dpu_ids = [
                machine_id(seed, MachineType::Dpu),
                machine_id(seed + 1, MachineType::Dpu),
            ];
            let mut setup = pool.begin().await?;
            for id in [host_id, dpu_ids[0], dpu_ids[1]] {
                super::create(setup.as_mut(), None, &id, ManagedHostState::Ready, None, 2).await?;
            }
            let ordered_dpus: Vec<MachineId> =
                sqlx::query_scalar("SELECT id FROM machines WHERE id = ANY($1) ORDER BY id")
                    .bind(dpu_ids.as_slice())
                    .fetch_all(setup.as_mut())
                    .await?;
            assert_eq!(ordered_dpus.len(), 2);
            let busy_id = ordered_dpus[0];
            let later_id = ordered_dpus[1];
            let segment_id: NetworkSegmentId = sqlx::query_scalar(
                "INSERT INTO network_segments (name, version)
                 VALUES ($1, 'V1-T0') RETURNING id",
            )
            .bind(format!("ipv6-loopback-busy-{seed}"))
            .fetch_one(setup.as_mut())
            .await?;
            sqlx::query(
                "INSERT INTO machine_interfaces
                    (machine_id, attached_dpu_machine_id, association_type,
                     segment_id, mac_address, primary_interface, hostname)
                 VALUES ($1, $2, 'Machine', $3, $4::macaddr, true, $5)",
            )
            .bind(host_id)
            .bind(busy_id)
            .bind(segment_id)
            .bind(format!("02:00:00:00:11:{seed:02x}"))
            .bind(format!("busy-backfill-{seed}"))
            .execute(setup.as_mut())
            .await?;
            setup.commit().await?;
            let original_host = super::get_network_config(&pool, &host_id).await?;
            let original_dpu = super::get_network_config(&pool, &busy_id).await?;

            // Each fresh attempt reserves before NOWAIT. Sequence increments
            // survive rollback, unlike the reservation rows being counted.
            let counter = format!(
                r#"
                CREATE SEQUENCE busy_backfill_attempts;
                CREATE FUNCTION count_busy_backfill_allocation()
                RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN
                    PERFORM nextval('busy_backfill_attempts');
                    RETURN NEW;
                END;
                $$;
                CREATE TRIGGER count_busy_backfill_allocation
                AFTER INSERT ON resource_pool_allocations
                FOR EACH ROW
                WHEN (NEW.pool_name = '{LOOPBACK_IP_V6}'
                      AND NEW.owner_type = 'machine' AND NEW.owner_id = '{busy_id}')
                EXECUTE FUNCTION count_busy_backfill_allocation();
                "#
            );
            // Interpolate only the internal pool name and canonical fixture ID.
            sqlx::raw_sql(sqlx::AssertSqlSafe(counter))
                .execute(&pool)
                .await?;

            // Hold either actual contention boundary through the retry budget.
            // Rejection must release the reservation and any completed target write.
            let mut group_lock = pool.begin().await?;
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR NO KEY UPDATE")
                .bind(if target_busy { busy_id } else { host_id })
                .fetch_one(group_lock.as_mut())
                .await?;
            let backfill = super::update_dpu_loopback_ips_v6(&pool, &common_pools);
            tokio::pin!(backfill);
            let reconciled =
                tokio::time::timeout(std::time::Duration::from_secs(10), &mut backfill).await;
            let at_gate = async {
                let host = super::get_network_config(&pool, &host_id).await?;
                let dpu = super::get_network_config(&pool, &busy_id).await?;
                let later = super::get_network_config(&pool, &later_id).await?;
                let stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
                let busy_allocations: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM resource_pool_allocations
                     WHERE pool_name = $1 AND owner_id = $2",
                )
                .bind(LOOPBACK_IP_V6)
                .bind(busy_id.to_string())
                .fetch_one(&pool)
                .await?;
                let attempts: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(pg_sequence_last_value('busy_backfill_attempts'), 0)",
                )
                .fetch_one(&pool)
                .await?;
                Ok::<_, Box<dyn std::error::Error>>((
                    host,
                    dpu,
                    later,
                    stats,
                    busy_allocations,
                    attempts,
                ))
            }
            .await;
            let released = group_lock.rollback().await;
            released?;
            let reconciled = match reconciled {
                Ok(result) => result,
                Err(error) => {
                    let late_result =
                        tokio::time::timeout(std::time::Duration::from_secs(30), &mut backfill)
                            .await?;
                    tracing::info!(
                        scenario,
                        ?late_result,
                        "Joined backfill after releasing its gate"
                    );
                    sqlx::raw_sql(DROP_ATTEMPT_COUNTER).execute(&pool).await?;
                    return Err(error.into());
                }
            };
            sqlx::raw_sql(DROP_ATTEMPT_COUNTER).execute(&pool).await?;
            let error =
                reconciled.expect_err("persistent contention must remain visible after the pass");
            match error {
                crate::DatabaseError::Sqlx(error) => assert_eq!(
                    error
                        .source
                        .as_database_error()
                        .and_then(|error| error.code())
                        .as_deref(),
                    Some("55P03"),
                    "{scenario}: retain the original NOWAIT error"
                ),
                error => panic!("{scenario}: expected a NOWAIT error, got {error:?}"),
            }

            let (
                host_after_contention,
                dpu_after_contention,
                later_config,
                stats,
                busy_allocations,
                attempts,
            ) = at_gate?;
            assert_eq!(
                attempts, 3,
                "{scenario}: each NOWAIT retry makes a fresh reservation"
            );
            assert_eq!(host_after_contention.value, original_host.value);
            assert_eq!(host_after_contention.version, original_host.version);
            assert_eq!(dpu_after_contention.value, original_dpu.value);
            assert_eq!(dpu_after_contention.version, original_dpu.version);
            let later_address = later_config
                .value
                .loopback_ip_v6
                .expect("an independent later DPU must still commit");
            assert_eq!(stats.used, pool_before.used + 1, "{scenario}");
            assert_eq!(stats.free, pool_before.free - 1, "{scenario}");
            assert_eq!(
                busy_allocations, 0,
                "failed attempts must release the reservation"
            );

            // Change the config before the next pass to check that recovery
            // preserves it.
            let mut change = pool.begin().await?;
            let (mut current_config, current_version) =
                super::get_network_config(change.as_mut(), &busy_id)
                    .await?
                    .take();
            current_config.use_admin_network = Some(false);
            assert_eq!(
                super::try_update_network_config(
                    change.as_mut(),
                    &busy_id,
                    current_version,
                    &current_config,
                )
                .await?,
                crate::ConditionalWrite::Applied(())
            );
            change.commit().await?;
            let changed_config = super::get_network_config(&pool, &busy_id).await?;
            super::update_dpu_loopback_ips_v6(&pool, &common_pools).await?;

            let host_after_retry = super::get_network_config(&pool, &host_id).await?;
            let dpu_after_retry = super::get_network_config(&pool, &busy_id).await?;
            let assigned = dpu_after_retry
                .value
                .loopback_ip_v6
                .expect("the next pass must backfill the released group");
            current_config.loopback_ip_v6 = Some(assigned);
            assert_eq!(dpu_after_retry.value, current_config);
            assert_eq!(host_after_retry.value, original_host.value);
            assert_eq!(host_after_retry.version, dpu_after_retry.version);
            assert_eq!(
                dpu_after_retry.version.version_nr(),
                changed_config.version.version_nr() + 1
            );
            assert_ne!(assigned, later_address);
            let later_after_retry = super::get_network_config(&pool, &later_id).await?;
            assert_eq!(later_after_retry.value, later_config.value);
            assert_eq!(later_after_retry.version, later_config.version);
            let stats = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
            assert_eq!(stats.used, pool_before.used + 2, "{scenario}");
            assert_eq!(stats.free, pool_before.free - 2, "{scenario}");
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn ipv6_loopback_backfill_preserves_completed_rows_when_pool_exhausts(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let common_pools = common_pools(&pool, Some("2001:db8:3::1/128")).await?;
        let dpu_machine_ids = [
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?,
            MachineId::from_str("fm100ds27v4uuq7sgs4gsjummskt0b3tedugtpevjrbfh6su081n9jufcq0")?,
        ];

        // Passing no common pools recreates rows from before the IPv6 pool was
        // configured without consuming its only address during insertion.
        let mut txn = pool.begin().await?;
        for dpu_machine_id in &dpu_machine_ids {
            super::create(
                txn.as_mut(),
                None,
                dpu_machine_id,
                ManagedHostState::Ready,
                None,
                2,
            )
            .await?;
        }
        txn.commit().await?;

        let error = super::update_dpu_loopback_ips_v6(&pool, &common_pools)
            .await
            .expect_err("one address cannot backfill two DPUs");
        assert!(matches!(error, crate::DatabaseError::ResourceExhausted(_)));

        let mut txn = pool.begin().await?;
        let configs_after_first_pass = [
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[0])
                .await?
                .value,
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[1])
                .await?
                .value,
        ];
        assert_eq!(
            configs_after_first_pass
                .iter()
                .filter(|config| config.loopback_ip_v6.is_some())
                .count(),
            1
        );
        txn.commit().await?;
        let stats_after_first_pass = crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?;
        assert_eq!(stats_after_first_pass.used, 1);
        assert_eq!(stats_after_first_pass.free, 0);

        let error = super::update_dpu_loopback_ips_v6(&pool, &common_pools)
            .await
            .expect_err("the remaining DPU must continue to report exhaustion");
        assert!(matches!(error, crate::DatabaseError::ResourceExhausted(_)));

        let mut txn = pool.begin().await?;
        let configs_after_second_pass = [
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[0])
                .await?
                .value,
            super::get_network_config(txn.as_mut(), &dpu_machine_ids[1])
                .await?
                .value,
        ];
        txn.commit().await?;
        assert_eq!(configs_after_second_pass, configs_after_first_pass);
        assert_eq!(
            crate::resource_pool::stats(&pool, LOOPBACK_IP_V6).await?,
            stats_after_first_pass
        );
        Ok(())
    }
}

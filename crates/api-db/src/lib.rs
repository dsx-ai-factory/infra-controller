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

// It's too cumbersome for tests to adhere to these, which are less important in testing anyway.
#![cfg_attr(test, allow(txn_held_across_await, txn_without_commit))]

pub mod attestation;
pub mod bmc_metadata;
pub mod bmc_redfish_session;
pub mod bmc_suppression;
pub mod carbide_version;
pub mod compute_allocation;
pub mod credential_rotation;
pub mod db_read;
pub mod desired_firmware;
pub mod dhcp_entry;
pub mod dhcp_record;
pub mod direct_dispatch_firmware_job;
pub mod dns;
pub mod dpa_interface;
pub mod dpu_agent_upgrade_policy;
pub mod dpu_machine_update;
pub mod dpu_remediation;
pub mod expected_machine;
pub mod expected_power_shelf;
pub mod expected_rack;
pub mod expected_switch;
pub mod explored_endpoints;
pub mod explored_managed_host;
pub mod extension_service;
pub mod health_history;
pub mod health_report;
pub mod host_firmware_config;
pub mod host_machine_update;
pub mod host_naming;
pub mod ib_partition;
pub mod instance;
pub mod instance_address;
pub mod instance_network_config;
pub mod instance_type;
pub mod ip_allocator;
pub mod machine;
pub mod machine_boot_override;
pub mod machine_desired_boot_interface;
pub mod machine_interface;
pub mod machine_interface_address;
pub mod machine_pending_action;
pub mod machine_topology;
pub mod machine_validation;
pub mod machine_validation_config;
pub mod machine_validation_execution;
pub mod machine_validation_result;
pub mod machine_validation_suites;
pub mod managed_host;
pub mod measured_boot;
pub mod migrations;
pub mod network_devices;
pub mod network_prefix;
pub mod network_security_group;
pub mod network_segment;
pub mod nvl_logical_partition;
pub mod nvl_partition;
pub mod nvlink_domain_health_report;
pub mod nvlink_nmxc_endpoints;
pub mod operating_system;
pub mod os_image;
pub mod power_options;
pub mod power_shelf;
pub mod predicted_machine_interface;
pub mod queries;
pub mod rack;
pub mod redfish_actions;
pub mod resource_pool;
pub mod retained_boot_interface;
pub mod retired_ib_membership;
pub mod route_servers;
pub mod secrets;
pub mod site_exploration_report;
pub mod site_explorer_run_status;
pub mod site_prefix;
pub mod sku;
pub mod spx_partition;
pub mod state_history;
pub mod switch;
pub mod tenant;
pub mod tenant_identity_config;
pub mod tenant_keyset;
pub mod tenant_prefix_overlap;
pub mod trim_table;
pub mod vpc;
pub mod vpc_dpu_loopback;
pub mod vpc_peering;
pub mod vpc_prefix;
pub mod work_lock_manager;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::convert::Infallible;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::pin::Pin;

use carbide_instrument::{Event, LabelValue, emit};
#[cfg(test)]
pub(crate) use carbide_macros::sqlx_test;
use carbide_uuid::machine::InvalidMachineType;
use mac_address::MacAddress;
use model::ConfigValidationError;
use model::hardware_info::HardwareInfoError;
use model::tenant::TenantError;
use sqlx::{Acquire, PgPool, PgTransaction, Postgres};
use tonic::Status;

use crate::ip_allocator::DhcpError;
use crate::machine_interface_address::AddressAlreadyInUseError;
use crate::resource_pool::ResourcePoolDatabaseError;

// Max values we can bind to a Postgres SQL statement... half the stated value of 65536, since in
// practice, 65535 query parameters seems to freeze up my postgres server in testing.
pub const BIND_LIMIT: usize = 32768;

/// A parameter to find() to filter resources based on an implied ID column
pub enum ObjectFilter<'a, ID> {
    /// Don't filter. Return all objects
    All,

    /// Filter by a list of uuids
    /// The filter will return any objects whose ID is included in the list.
    /// If the list is empty, the filter will return no objects.
    List(&'a [ID]),

    /// Retrieve a single objects
    One(ID),
}

/// A parameter to find_by() to filter resources based on a specified column
#[derive(Clone)]
pub enum ObjectColumnFilter<'a, C: ColumnInfo<'a>> {
    /// Don't filter. Return all objects
    All,

    /// Filter where column = ANY([T])
    ///
    /// The filter will return any objects where the value of the column C is
    /// included in this list [T]. If the list is empty, the filter will return no
    /// objects.
    List(C, &'a [C::ColumnType]),

    /// Retrieve a single object where the value of the column C is equal to T
    One(C, &'a C::ColumnType),
}

/// Newtype wrapper around sqlx::QueryBuilder that allows passing an ObjectColumnFilter to build the WHERE clause
pub struct FilterableQueryBuilder(sqlx::QueryBuilder<Postgres>);

impl FilterableQueryBuilder {
    pub fn new(init: impl Into<String>) -> Self {
        FilterableQueryBuilder(sqlx::QueryBuilder::new(init))
    }

    /// Push a WHERE clause to this query builder that matches the given filter, optionally using
    /// the given relation to qualify the column names
    pub fn filter_relation<'a, C: ColumnInfo<'a>>(
        mut self,
        filter: &ObjectColumnFilter<'a, C>,
        relation: Option<&str>,
    ) -> sqlx::QueryBuilder<Postgres> {
        match filter {
            ObjectColumnFilter::All => self.0.push(" WHERE true".to_string()),
            ObjectColumnFilter::List(column, list) => {
                if let Some(relation) = relation {
                    self.0
                        .push(format!(" WHERE {}.{}=ANY(", relation, column.column_name()))
                        .push_bind(*list)
                        .push(")")
                } else {
                    self.0
                        .push(format!(" WHERE {}=ANY(", column.column_name()))
                        .push_bind(*list)
                        .push(")")
                }
            }
            ObjectColumnFilter::One(column, id) => {
                if let Some(relation) = relation {
                    self.0
                        .push(format!(" WHERE {}.{}=", relation, column.column_name()))
                        .push_bind(*id)
                } else {
                    self.0
                        .push(format!(" WHERE {}=", column.column_name()))
                        .push_bind(*id)
                }
            }
        };

        self.0
    }

    /// Push a WHERE clause to this query builder that matches the given filter.
    pub fn filter<'a, C: ColumnInfo<'a>>(
        self,
        filter: &ObjectColumnFilter<'a, C>,
    ) -> sqlx::QueryBuilder<Postgres> {
        self.filter_relation(filter, None)
    }
}

#[test]
fn test_filter_relation() {
    use crate::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter};

    #[derive(Copy, Clone)]
    struct IdColumn;
    impl ColumnInfo<'_> for IdColumn {
        type TableType = ();
        type ColumnType = i32;
        fn column_name(&self) -> &'static str {
            "id"
        }
    }

    let query = FilterableQueryBuilder::new("SELECT * from table1 t")
        .filter_relation(&ObjectColumnFilter::One(IdColumn, &1), Some("t"));
    assert_eq!(query.sql(), "SELECT * from table1 t WHERE t.id=$1");
}

#[test]
fn test_filter() {
    use crate::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter};

    #[derive(Copy, Clone)]
    struct IdColumn;
    impl ColumnInfo<'_> for IdColumn {
        type TableType = ();
        type ColumnType = i32;
        fn column_name(&self) -> &'static str {
            "id"
        }
    }

    let query = FilterableQueryBuilder::new("SELECT * from table1")
        .filter(&ObjectColumnFilter::One(IdColumn, &1));
    assert_eq!(query.sql(), "SELECT * from table1 WHERE id=$1");
}

/// Metadata about a particular column that can be filtered by in a typical `find_by` function
///
/// This conveys metadata such as the name of the column and the type of data it returns, so that we
/// can write generic functions to build SQL queries from given search criteria, while maintaining
/// type safety.
pub trait ColumnInfo<'a>: Clone + Copy {
    /// TableType has no requirements, it is here to allow `find_by` functions to constrain what
    /// columns can be searched by, via type bounds. For example, this will fail to compile:
    ///
    /// ```ignore
    /// use crate::{ColumnInfo, ObjectColumnFilter};
    ///
    /// struct GoodTable; // Marker type, can be otherwise unused
    /// struct BadTable; // Marker type, can be otherwise unused
    ///
    /// #[derive(Copy, Clone)]
    /// struct GoodColumn;
    /// impl <'a> ColumnInfo<'a> for GoodColumn {
    ///     type TableType = GoodTable;
    ///     type ColumnType = &'a str;
    ///     fn column_name(&self) -> &'static str { "id" }
    /// }
    ///
    /// #[derive(Copy, Clone)]
    /// struct BadColumn;
    /// impl <'a> ColumnInfo<'a> for BadColumn {
    ///     type TableType = BadTable;
    ///     type ColumnType = &'a str;
    ///     fn column_name(&self) -> &'static str { "id" }
    /// }
    ///
    /// fn find_by<'a, C: ColumnInfo<'a, TableType=GoodTable>>(
    ///     filter: ObjectColumnFilter<'a, C>
    /// ) {}
    ///
    /// find_by(ObjectColumnFilter::One(BadColumn, &"hello")) // error[E0271]: type mismatch resolving `<BadColumn as ColumnInfo<'_>>::TableType == GoodTable`
    /// ```
    type TableType;
    type ColumnType: sqlx::Type<sqlx::Postgres>
        + Send
        + Sync
        + sqlx::Encode<'a, sqlx::Postgres>
        + sqlx::postgres::PgHasArrayType;
    fn column_name(&self) -> &'static str;
}

///
/// Wraps a sqlx::Error and records location and query
#[derive(Debug)]
pub struct AnnotatedSqlxError {
    file: &'static str,
    line: u32,
    query: String,
    pub source: sqlx::Error,
}

impl AnnotatedSqlxError {
    #[track_caller]
    pub fn new(op_name: impl AsRef<str>, source: sqlx::Error) -> Self {
        let loc = Location::caller();
        AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: op_name.as_ref().to_string(),
            source,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum DatabaseError {
    #[error("generic error from report: {0}")]
    GenericErrorFromReport(#[from] eyre::ErrReport),
    #[error(transparent)]
    Sqlx(#[from] AnnotatedSqlxError),
    #[error("{kind} not found: {id}")]
    NotFoundError {
        /// The type of the resource that was not found (e.g. Machine)
        kind: &'static str,
        /// The ID of the resource that was not found
        id: String,
    },
    #[error("internal error: {message}")]
    Internal { message: String },
    #[error("unable to parse string into IP address: {0}")]
    AddressParseError(#[from] std::net::AddrParseError),
    #[error(transparent)]
    AddressAlreadyInUse(#[from] AddressAlreadyInUseError),
    #[error("unable to parse string into IP network: {0}")]
    NetworkParseError(#[from] ipnetwork::IpNetworkError),
    #[error("{kind} already exists: {id}")]
    AlreadyFoundError {
        /// The type of the resource that already exists (e.g. Machine)
        kind: &'static str,
        /// The ID of the resource that already exists.
        id: String,
    },
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    #[error("duplicate MAC address for expected host BMC interface: {0}")]
    ExpectedHostDuplicateMacAddress(MacAddress),
    #[error("NVOS MAC address is already claimed by another expected switch: {0}")]
    ExpectedSwitchDuplicateNvosMacAddress(MacAddress),
    #[error("argument is missing in input: {0}")]
    MissingArgument(&'static str),
    #[error("uuid type conversion error: {0}")]
    UuidConversionError(#[from] uuid::Error),
    #[error("RPC uuid type conversion error: {0}")]
    RpcUuidConversionError(#[from] carbide_uuid::UuidConversionError),
    #[error(
        "an object of type {0} was intended to be modified did not have the expected version {1}"
    )]
    ConcurrentModificationError(&'static str, String),
    #[error("{0}")]
    FailedPrecondition(String),
    #[error("all network segments are not allocated yet")]
    NetworkSegmentNotAllocated,
    #[error("find one returned no results but should return one for uuid - {0}")]
    FindOneReturnedNoResultsError(uuid::Uuid),
    #[error("find one returned many results but should return one for uuid - {0}")]
    FindOneReturnedManyResultsError(uuid::Uuid),
    #[error("resource {0} is empty")]
    ResourceExhausted(String),
    #[error("tenant SitePrefix quota reached: {used} of {limit} retained SitePrefixes are in use")]
    TenantSitePrefixQuotaExceeded { used: u32, limit: u32 },
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(#[from] ConfigValidationError),
    #[error("resource pool error: {0}")]
    ResourcePoolError(#[from] model::resource_pool::ResourcePoolError),
    #[error("only one interface per machine can be marked as primary")]
    OnePrimaryInterface,
    #[error("duplicate MAC address for network: {0}")]
    NetworkSegmentDuplicateMacAddress(MacAddress),
    #[error("admin network is not configured")]
    AdminNetworkNotConfigured,
    #[error("network has attached VPC or subdomain : {0}")]
    NetworkSegmentDelete(String),
    #[error("tenant handling error: {0}")]
    TenantError(#[from] TenantError),
    #[error("hardware info error: {0}")]
    HardwareInfoError(#[from] HardwareInfoError),
    #[error("the function is not implemented")]
    NotImplemented,
    #[error("error in DHCP allocation/handling: {0}")]
    DhcpError(#[from] DhcpError),
    #[error("maximum one association per interface")]
    MaxOneInterfaceAssociation,
    #[error("fast-path allocation failed and can be retried")]
    TryAgain,
    #[error("no site-wide rotation target for credential type: {0:?}")]
    MissingSitewideRotationTarget(crate::credential_rotation::CredentialRotationType),
}

// Implement From<Infallible> so that we can write generic code that converts between MachineId
// representations using TryFrom, while supporting "non-converting" cases where the caller is
// already passing the right type. (The `impl TryFrom<T> for T` blanket impl uses `Infallible` as
// its error type.)
impl From<Infallible> for DatabaseError {
    fn from(_: Infallible) -> Self {
        // We just crash if this is ever called, because the whole point of `Infallible` is that
        // it's an error variant that is never actually constructed. Future rust versions will use
        // `!` as the error type for TryFrom<T> for T, and this whole conversion will become
        // unnecessary (as the compiler will already determine it to be unreachable.)
        unreachable!()
    }
}

impl From<InvalidMachineType> for DatabaseError {
    fn from(value: InvalidMachineType) -> Self {
        Self::internal(value.to_string())
    }
}

impl DatabaseError {
    /// Returns true if the database error wrapps a sqlx::Error::RowNotFound, or if it's our own DatabaseError::NotFoundError
    pub fn is_not_found(&self) -> bool {
        match self {
            DatabaseError::Sqlx(e) => matches!(e.source, sqlx::Error::RowNotFound),
            DatabaseError::NotFoundError { .. } => true,
            _ => false,
        }
    }

    pub fn with_op_name(self, op_name: &str) -> Self {
        match self {
            DatabaseError::Sqlx(e) => DatabaseError::new(op_name, e.source),
            _ => self,
        }
    }

    pub fn is_fqdn_conflict(&self) -> bool {
        match self {
            DatabaseError::Sqlx(sqlx_error) => match &sqlx_error.source {
                sqlx::Error::Database(database_error) => {
                    database_error.constraint() == Some("fqdn_must_be_unique")
                }
                _ => false,
            },
            _ => false,
        }
    }
}

pub type DatabaseResult<T> = Result<T, DatabaseError>;

impl DatabaseError {
    #[track_caller]
    pub fn new(op_name: impl AsRef<str>, source: sqlx::Error) -> DatabaseError {
        let loc = Location::caller();
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: op_name.as_ref().to_string(),
            source,
        })
    }

    fn txn_begin(source: sqlx::Error, loc: &'static Location<'static>) -> DatabaseError {
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: "transaction begin".into(),
            source,
        })
    }

    fn txn_commit(source: sqlx::Error, loc: &'static Location<'static>) -> DatabaseError {
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: "transaction commit".into(),
            source,
        })
    }

    fn txn_rollback(source: sqlx::Error, loc: &'static Location<'static>) -> DatabaseError {
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: "transaction rollback".into(),
            source,
        })
    }

    #[track_caller]
    pub fn acquire(source: sqlx::Error) -> DatabaseError {
        let loc = Location::caller();
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: "acquire connection".into(),
            source,
        })
    }

    #[track_caller]
    pub fn query(query: impl AsRef<str>, source: sqlx::Error) -> DatabaseError {
        let loc = Location::caller();
        DatabaseError::Sqlx(AnnotatedSqlxError {
            file: loc.file(),
            line: loc.line(),
            query: query.as_ref().to_string(),
            source,
        })
    }

    /// Creates a `Internal` error with the given error message
    pub fn internal(message: String) -> Self {
        DatabaseError::Internal { message }
    }
}

impl Display for AnnotatedSqlxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Database Error: {} file={} line={} query={}.",
            self.source, self.file, self.line, self.query,
        )
    }
}

impl Error for AnnotatedSqlxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

impl From<DatabaseError> for tonic::Status {
    fn from(from: DatabaseError) -> Self {
        // If env RUST_BACKTRACE is set extract handler and err location
        // If it's not set `Backtrace::capture()` is very cheap to call
        let b = Backtrace::capture();
        let printed = if b.status() == BacktraceStatus::Captured {
            let b_str = b.to_string();
            let f = b_str
                .lines()
                .skip(1)
                .skip_while(|l| !l.contains("carbide"))
                .take(2)
                .collect::<Vec<&str>>();
            if f.len() == 2 {
                let handler = f[0].trim();
                let location = f[1].trim().replace("at ", "");
                tracing::error!(
                    error = %from,
                    error_location = %location,
                    handler,
                    "database error conversion",
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        if !printed {
            match from {
                DatabaseError::NotImplemented => {}
                _ => tracing::error!(error = %from, "database error conversion"),
            }
        }

        match &from {
            DatabaseError::AddressParseError(e) => Status::invalid_argument(e.to_string()),
            error @ DatabaseError::AlreadyFoundError { .. } => {
                Status::failed_precondition(error.to_string())
            }
            error @ DatabaseError::ConcurrentModificationError(_, _) => {
                Status::failed_precondition(error.to_string())
            }
            error @ DatabaseError::ExpectedHostDuplicateMacAddress(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ DatabaseError::FailedPrecondition(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ DatabaseError::Internal { .. } => Status::internal(error.to_string()),
            DatabaseError::InvalidArgument(msg) => Status::invalid_argument(msg),
            DatabaseError::InvalidConfiguration(e) => Status::invalid_argument(e.to_string()),
            error @ DatabaseError::DhcpError(_) => Status::resource_exhausted(error.to_string()),
            DatabaseError::MissingArgument(msg) => Status::invalid_argument(*msg),
            DatabaseError::NetworkParseError(e) => Status::invalid_argument(e.to_string()),
            DatabaseError::NetworkSegmentDelete(msg) => Status::invalid_argument(msg),
            DatabaseError::NotFoundError { kind, id } => {
                Status::not_found(format!("{kind} not found: {id}"))
            }
            DatabaseError::ResourceExhausted(kind) => Status::resource_exhausted(kind),
            error @ DatabaseError::TenantSitePrefixQuotaExceeded { .. } => {
                Status::resource_exhausted(error.to_string())
            }
            error @ DatabaseError::RpcUuidConversionError(_) => {
                Status::invalid_argument(error.to_string())
            }
            error @ DatabaseError::UuidConversionError(_) => {
                Status::invalid_argument(error.to_string())
            }
            other => Status::internal(other.to_string()),
        }
    }
}

// MARK: - Custom DatabaseError From<> impls to flatten error variants

impl From<ResourcePoolDatabaseError> for DatabaseError {
    fn from(from: ResourcePoolDatabaseError) -> Self {
        match from {
            ResourcePoolDatabaseError::ResourcePool(e) => DatabaseError::ResourcePoolError(e),
            ResourcePoolDatabaseError::Database(e) => *e,
        }
    }
}
impl From<::measured_boot::Error> for DatabaseError {
    fn from(value: ::measured_boot::Error) -> Self {
        DatabaseError::internal(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum DatabaseTransactionRollbackTrigger {
    ExplicitCleanup,
    ClosureError,
}

#[derive(Event)]
#[event(
    event_name = "database_transaction_rollback_failed",
    metric_name = "carbide_database_transaction_rollback_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "Failed to roll back transaction",
    describe = "Number of database transaction rollback failures, by trigger."
)]
struct DatabaseTransactionRollbackFailed {
    #[label]
    trigger: DatabaseTransactionRollbackTrigger,
    #[context]
    caller: String,
    #[context]
    context: &'static str,
    #[context]
    error: String,
}

impl DatabaseTransactionRollbackFailed {
    fn new(
        trigger: DatabaseTransactionRollbackTrigger,
        caller: &'static Location<'static>,
        context: &'static str,
        error: &DatabaseError,
    ) -> Self {
        Self {
            trigger,
            caller: caller.to_string(),
            context,
            error: error.to_string(),
        }
    }
}

fn record_database_transaction_rollback_failure(
    trigger: DatabaseTransactionRollbackTrigger,
    caller: &'static Location<'static>,
    context: &'static str,
    error: sqlx::Error,
) {
    let error = DatabaseError::txn_rollback(error, caller);
    emit(DatabaseTransactionRollbackFailed::new(
        trigger, caller, context, &error,
    ));
}

#[derive(Debug)]
pub struct Transaction<'a> {
    inner: sqlx::PgTransaction<'a>,
}

impl<'a> AsMut<sqlx::PgTransaction<'a>> for Transaction<'a> {
    #[inline]
    fn as_mut(&mut self) -> &mut sqlx::PgTransaction<'a> {
        &mut self.inner
    }
}

impl<'a> Transaction<'a> {
    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub fn begin(pool: &'a sqlx::PgPool) -> impl Future<Output = Result<Self, DatabaseError>> {
        let loc = Location::caller();
        async move {
            pool.begin()
                .await
                .map_err(|e| DatabaseError::txn_begin(e, loc))
                .map(|inner| Self { inner })
        }
    }

    pub async fn begin_with_location(
        pool: &sqlx::PgPool,
        loc: &'static Location<'static>,
    ) -> Result<Self, DatabaseError> {
        pool.begin()
            .await
            .map_err(|e| DatabaseError::txn_begin(e, loc))
            .map(|inner| Self { inner })
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub fn begin_inner(
        conn: &'a mut sqlx::PgConnection,
    ) -> Pin<Box<dyn Future<Output = Result<Self, DatabaseError>> + Send + 'a>> {
        let loc = Location::caller();
        Box::pin(async move {
            conn.begin()
                .await
                .map_err(|e| DatabaseError::txn_begin(e, loc))
                .map(|inner| Self { inner })
        })
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub fn commit(self) -> Pin<Box<dyn Future<Output = Result<(), DatabaseError>> + Send + 'a>> {
        let loc = Location::caller();
        Box::pin(async move {
            self.inner
                .commit()
                .await
                .map_err(|e| DatabaseError::txn_commit(e, loc))
        })
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    #[track_caller]
    pub fn rollback(self) -> Pin<Box<dyn Future<Output = Result<(), DatabaseError>> + Send + 'a>> {
        let loc = Location::caller();
        Box::pin(async move {
            self.inner
                .rollback()
                .await
                .map_err(|e| DatabaseError::txn_rollback(e, loc))
        })
    }

    // This function can just async when
    // https://github.com/rust-lang/rust/issues/110011 will be
    // implemented
    /// Roll back the transaction, logging a warning tagged with `context` if
    /// the rollback itself fails, rather than propagating -- for read-only or
    /// cleanup paths where the caller can't act on a rollback error but must
    /// not swallow it.
    #[track_caller]
    pub fn rollback_or_log(
        self,
        context: &'static str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let loc = Location::caller();
        Box::pin(async move {
            if let Err(error) = self.inner.rollback().await {
                record_database_transaction_rollback_failure(
                    DatabaseTransactionRollbackTrigger::ExplicitCleanup,
                    loc,
                    context,
                    error,
                );
            }
        })
    }

    pub fn as_pgconn(&mut self) -> &mut sqlx::PgConnection {
        &mut self.inner
    }
}

impl<'a> Deref for Transaction<'a> {
    type Target = sqlx::PgTransaction<'a>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Transaction<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Provides an easy way to auto-commit a transaction if no error is thrown.
pub trait WithTransaction {
    #[track_caller]
    fn with_txn<'pool, T, E>(
        &'pool self,
        f: impl for<'txn> FnOnce(
            &'txn mut PgTransaction<'pool>,
        ) -> futures::future::BoxFuture<'txn, Result<T, E>>
        + Send,
    ) -> impl Future<Output = DatabaseResult<Result<T, E>>> + Send
    where
        T: Send,
        E: Send;
}

impl WithTransaction for PgPool {
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
        let caller = Location::caller();
        async move {
            let mut t = self
                .begin()
                .await
                .map_err(|e| DatabaseError::txn_begin(e, caller))?;
            match f(&mut t).await {
                Ok(output) => {
                    t.commit()
                        .await
                        .map_err(|e| DatabaseError::txn_commit(e, caller))?;
                    Ok(Ok(output))
                }
                Err(e) => {
                    if let Err(error) = t.rollback().await {
                        record_database_transaction_rollback_failure(
                            DatabaseTransactionRollbackTrigger::ClosureError,
                            caller,
                            "transaction closure returned an error",
                            error,
                        );
                    }
                    Ok(Err(e))
                }
            }
        }
    }
}

pub trait TransactionVending {
    fn txn_begin(&self) -> impl Future<Output = Result<Transaction<'_>, DatabaseError>>;
}

impl TransactionVending for PgPool {
    #[track_caller]
    // This returns an `impl Future` instead of being async, so that we can use #[track_caller],
    // which is unsupported with async fn's.
    fn txn_begin(&self) -> impl Future<Output = Result<Transaction<'_>, DatabaseError>> {
        Transaction::begin(self)
    }
}

#[cfg(test)]
#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    carbide_test_support::setup_test_logging("api-db");
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::ip_allocator::DhcpError;

    const TRANSACTION_ROLLBACK_FAILURES_METRIC: &str =
        "carbide_database_transaction_rollback_failures_total";

    #[derive(Clone, Copy)]
    struct RollbackFailureCase {
        trigger: DatabaseTransactionRollbackTrigger,
        context: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct RollbackFailureRecord {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        trigger: Option<String>,
        caller: Option<String>,
        context: Option<String>,
        error: Option<String>,
        counter_delta: f64,
    }

    #[test]
    fn database_transaction_rollback_failures_log_and_count_by_trigger() {
        let caller = Location::caller();

        check_values(
            [
                Check {
                    scenario: "explicit cleanup",
                    input: RollbackFailureCase {
                        trigger: DatabaseTransactionRollbackTrigger::ExplicitCleanup,
                        context: "read-only cleanup",
                    },
                    expect: RollbackFailureRecord {
                        metadata_name: "database_transaction_rollback_failed".to_string(),
                        level: tracing::Level::WARN,
                        message: "Failed to roll back transaction".to_string(),
                        event_name: Some("database_transaction_rollback_failed".to_string()),
                        metric_name: Some(TRANSACTION_ROLLBACK_FAILURES_METRIC.to_string()),
                        trigger: Some("explicit_cleanup".to_string()),
                        caller: Some(caller.to_string()),
                        context: Some("read-only cleanup".to_string()),
                        error: Some("internal error: rollback unavailable".to_string()),
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "closure error",
                    input: RollbackFailureCase {
                        trigger: DatabaseTransactionRollbackTrigger::ClosureError,
                        context: "transaction closure returned an error",
                    },
                    expect: RollbackFailureRecord {
                        metadata_name: "database_transaction_rollback_failed".to_string(),
                        level: tracing::Level::WARN,
                        message: "Failed to roll back transaction".to_string(),
                        event_name: Some("database_transaction_rollback_failed".to_string()),
                        metric_name: Some(TRANSACTION_ROLLBACK_FAILURES_METRIC.to_string()),
                        trigger: Some("closure_error".to_string()),
                        caller: Some(caller.to_string()),
                        context: Some("transaction closure returned an error".to_string()),
                        error: Some("internal error: rollback unavailable".to_string()),
                        counter_delta: 1.0,
                    },
                },
            ],
            |case| {
                let metrics = MetricsCapture::start();
                let error = DatabaseError::Internal {
                    message: "rollback unavailable".to_string(),
                };
                let logs = capture_logs(|| {
                    emit(DatabaseTransactionRollbackFailed::new(
                        case.trigger,
                        caller,
                        case.context,
                        &error,
                    ));
                });

                assert_eq!(logs.len(), 1, "one emit must produce one log record");
                let log = &logs[0];
                let trigger = case.trigger.label_value();

                RollbackFailureRecord {
                    metadata_name: log.metadata_name.clone(),
                    level: log.level,
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    trigger: log.field("trigger").map(str::to_string),
                    caller: log.field("caller").map(str::to_string),
                    context: log.field("context").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    counter_delta: metrics.counter_delta(
                        TRANSACTION_ROLLBACK_FAILURES_METRIC,
                        &[("trigger", trigger.as_str())],
                    ),
                }
            },
        );
    }

    #[test]
    fn test_database_error_new() {
        const OP_NAME: &str = "something people want to say";
        let DatabaseError::Sqlx(err) =
            DatabaseError::new(OP_NAME, sqlx::Error::protocol("some error"))
        else {
            unreachable!()
        };
        assert_eq!(err.line, line!() - 4);
        assert_eq!(err.file, file!());
        assert!(format!("{err}").contains(OP_NAME))
    }

    #[test]
    fn test_database_error_query() {
        const DB_QUERY: &str = "SELECT * from some_table;";
        let DatabaseError::Sqlx(err) =
            DatabaseError::query(DB_QUERY, sqlx::Error::protocol("some error"))
        else {
            unreachable!()
        };
        assert_eq!(err.line, line!() - 4);
        assert_eq!(err.file, file!());
        assert!(format!("{err}").contains(DB_QUERY));
    }

    #[test]
    fn test_dhcp_error_maps_to_resource_exhausted_status() {
        let err = DatabaseError::DhcpError(DhcpError::PrefixExhausted(
            "10.217.5.160".parse().expect("valid IP"),
        ));
        let status: tonic::Status = err.into();
        assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    }
}

// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::net::IpAddr;
use std::time::Duration;

use ::rpc::forge::component_power_observation::Outcome as PowerOutcome;
use ::rpc::forge::get_component_power_details_request::Target;
use ::rpc::forge::{self as rpc};
use component_manager::component_common::{ComponentPowerStateResult, PowerStateSource};
use component_manager::compute_tray_manager::{
    ComputeTrayEndpoint, ComputeTrayManager, ComputeTrayVendor,
};
use component_manager::core_compute_manager::CoreComputeTrayManager;
use component_manager::error::ComponentManagerError;
use component_manager::nv_switch_manager::SwitchEndpoint;
use component_manager::power_shelf_manager::{PowerShelfEndpoint, PowerShelfVendor};
use futures_util::{StreamExt, stream};
use mac_address::MacAddress;
use model::machine::PowerState;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::{Request, Response, Status};

use super::{
    fetch_compute_tray_bmc_credentials, fetch_powershelf_pmc_credentials,
    fetch_switch_bmc_credentials, fetch_switch_nvos_credentials, is_rack_scale_server,
};
use crate::CarbideError;
use crate::api::{Api, log_request_data};

mod actions;

// Bound request memory and hardware fan-out. The five-second snapshot budget
// includes pool acquisition. `SNAPSHOT_TIMEOUT` also caps each server statement
// after snapshot setup, independently of that overall client deadline. After
// the snapshot completes, queued targets, credentials, and hardware reads share
// fifteen seconds; no portion is reserved per target.
// Caller cancellation can stop work sooner, as can Core/RMS's ten-second
// backend read budgets.
const MAX_TARGETS: usize = 64;
const READ_CONCURRENCY: usize = 8;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// `PowerReadEndpoint` keeps the management identity resolved in the snapshot.
/// Credentials are fetched only after that transaction has been released.
enum PowerReadEndpoint {
    Compute {
        ip: IpAddr,
        mac: MacAddress,
        vendor: ComputeTrayVendor,
        // Standalone hosts use Core directly instead of the configured rack backend.
        standalone: bool,
    },
    Switch(db::switch::SwitchEndpointRow),
    Shelf(db::power_shelf::PowerShelfEndpointRow),
}

/// `PendingDetails` keeps identity, policy and actions while hardware is read.
/// Without an endpoint, its existing error and saved details return unchanged.
struct PendingDetails {
    details: rpc::ComponentPowerDetails,
    endpoint: Option<PowerReadEndpoint>,
}

/// `DbSessionGuard` owns a pooled connection until successful work explicitly
/// returns it through `return_to_pool`. Dropping the guard before that discards
/// the connection and immediately releases the local pool slot.
///
/// SQLx's normal cleanup can wait for a blocked query before rollback, so the
/// guard also covers setup and network stalls that the statement cap cannot.
struct DbSessionGuard {
    connection: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
}

impl DbSessionGuard {
    /// Wraps the pooled connection before `BEGIN`, so failed or cancelled
    /// transaction startup still releases its pool slot.
    async fn acquire(pool: &sqlx::PgPool) -> Result<Self, db::DatabaseError> {
        Ok(Self {
            connection: Some(pool.acquire().await.map_err(db::DatabaseError::acquire)?),
        })
    }

    /// Starts a consistent, read-only snapshot with a transaction-local server
    /// limit of at most `SNAPSHOT_TIMEOUT` per subsequent statement. A shorter
    /// operator setting remains in effect. The separate client deadline covers
    /// acquisition and setup; the server limit does not cover those stages.
    async fn begin(&mut self) -> Result<db::Transaction<'_>, db::DatabaseError> {
        let connection = self
            .connection
            .as_deref_mut()
            .expect("the snapshot retains its connection until return_to_pool consumes it");
        let mut txn = db::Transaction::begin_inner(connection).await?;
        // Policy, identity and maintenance must share a snapshot, without row locks.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(txn.as_pgconn())
            .await
            .map_err(|error| db::DatabaseError::query("set power details snapshot", error))?;

        // `pg_settings` reports milliseconds. `NULLIF` turns a disabled (zero)
        // timeout into NULL, which `LEAST` ignores when applying our cap. A
        // shorter limit remains unchanged. The `true` argument makes this
        // transaction-local, so the setting resets when the txn ends.
        sqlx::query(
            "SELECT set_config('statement_timeout', \
                LEAST(NULLIF(setting::bigint, 0), $1::text::bigint)::text, true) \
             FROM pg_settings WHERE name = 'statement_timeout'",
        )
        .bind(SNAPSHOT_TIMEOUT.as_millis().to_string())
        .execute(txn.as_pgconn())
        .await
        .map_err(|error| db::DatabaseError::query("set power details statement timeout", error))?;
        Ok(txn)
    }

    /// Returns a healthy connection through SQLx after its transaction commits.
    fn return_to_pool(mut self) {
        drop(self.connection.take());
    }
}

impl Drop for DbSessionGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detaching frees the local pool slot immediately. Closing the
            // socket does not guarantee that PostgreSQL stops immediately;
            // after snapshot setup, its statement timeout separately bounds
            // each subsequent SQL statement. Discarded server sessions may
            // briefly remain alongside replacements and exceed the pool limit.
            drop(connection.detach());
        }
    }
}

/// Loads identity, policy and tracked maintenance in one snapshot, then releases
/// it before bounded credential and hardware reads. Saved details remain intact.
pub(crate) async fn get_component_power_details(
    api: &Api,
    request: Request<rpc::GetComponentPowerDetailsRequest>,
) -> Result<Response<rpc::GetComponentPowerDetailsResponse>, Status> {
    log_request_data(&request);
    let target = request
        .into_inner()
        .target
        .ok_or(CarbideError::MissingArgument("target is required"))?;
    validate_target(&target)?;

    let snapshot = tokio::time::timeout(SNAPSHOT_TIMEOUT, load_snapshot(api, target))
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "power details database snapshot deadline exceeded");
            CarbideError::Internal {
                message: "power details database snapshot failed".into(),
            }
        })?
        .map_err(|error| {
            // Database conversion errors can contain persisted values. Keep
            // diagnostics categorical rather than logging arbitrary payloads.
            let error_category = match &error {
                CarbideError::DBError(error) => match &error.source {
                    sqlx::Error::PoolTimedOut => "pool_timeout",
                    sqlx::Error::PoolClosed => "pool_closed",
                    sqlx::Error::Database(_) => "database",
                    sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode",
                    _ => "database_io",
                },
                CarbideError::DatabaseTypeConversionError(_) => "model_conversion",
                _ => "snapshot",
            };
            tracing::warn!(error_category, "power details database snapshot failed");
            CarbideError::Internal {
                message: "power details database snapshot failed".into(),
            }
        })?;

    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    // Start another target whenever a slot opens, even if an earlier target is
    // still waiting on credentials or hardware. Restore request order below.
    let mut entries = stream::iter(snapshot.into_iter().enumerate())
        .map(|(index, mut pending)| async move {
            if let Some(endpoint) = pending.endpoint {
                // Live-read failures replace only actual; saved policy and
                // tracked actions remain available from the snapshot.
                // An already-ready future can finish before timeout_at checks
                // expiry; queued targets must not start work after the deadline.
                pending.details.actual = Some(if tokio::time::Instant::now() >= deadline {
                    failed_observation(rpc::ComponentPowerErrorCode::DeadlineExceeded, None)
                } else {
                    match tokio::time::timeout_at(deadline, read_actual(api, endpoint, deadline))
                        .await
                    {
                        Ok(actual) => actual,
                        Err(_) => {
                            failed_observation(rpc::ComponentPowerErrorCode::DeadlineExceeded, None)
                        }
                    }
                });
            }
            (index, pending.details)
        })
        .buffer_unordered(READ_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    entries.sort_unstable_by_key(|(index, _)| *index);
    let entries = entries.into_iter().map(|(_, details)| details).collect();
    Ok(Response::new(rpc::GetComponentPowerDetailsResponse {
        entries,
    }))
}

/// Applies the same batch bounds and uniqueness rules to each typed target kind.
fn validate_target(target: &Target) -> Result<(), CarbideError> {
    match target {
        Target::MachineIds(list) => validate_ids(&list.machine_ids),
        Target::SwitchIds(list) => validate_ids(&list.ids),
        Target::PowerShelfIds(list) => validate_ids(&list.ids),
    }
}

/// Rejects out-of-bounds batches before allocating the borrowed uniqueness set.
fn validate_ids<Id: Eq + Hash>(ids: &[Id]) -> Result<(), CarbideError> {
    if ids.is_empty() || ids.len() > MAX_TARGETS {
        return Err(CarbideError::InvalidArgument(format!(
            "power details requires 1-{MAX_TARGETS} IDs; received {}",
            ids.len()
        )));
    }
    if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err(CarbideError::InvalidArgument(
            "power details IDs must be unique".into(),
        ));
    }
    Ok(())
}

/// `load_snapshot` reads identity, policy and maintenance in request order within
/// one read-only transaction, releasing it before credentials or hardware I/O.
async fn load_snapshot(api: &Api, target: Target) -> Result<Vec<PendingDetails>, CarbideError> {
    let mut connection = DbSessionGuard::acquire(api.pg_pool()).await?;
    let mut txn = connection.begin().await?;
    let entries = match target {
        Target::MachineIds(list) => {
            let machines = db::machine::find(
                txn.as_pgconn(),
                db::ObjectFilter::List(&list.machine_ids),
                MachineSearchConfig::default(),
            )
            .await?;
            let machines: HashMap<_, _> = machines
                .into_iter()
                .map(|machine| (machine.id, machine))
                .collect();
            let policies =
                db::power_options::get_by_ids(&list.machine_ids, txn.as_pgconn()).await?;
            let policies: HashMap<_, _> = policies
                .into_iter()
                .map(|policy| (policy.host_id, policy.desired_power_state))
                .collect();
            list.machine_ids
                .into_iter()
                .map(|id| {
                    let Some(machine) = machines.get(&id) else {
                        return PendingDetails::missing(id.to_string());
                    };
                    let endpoint = machine
                        .status
                        .bmc_info
                        .ip
                        .zip(machine.status.bmc_info.mac)
                        .map(|(ip, mac)| PowerReadEndpoint::Compute {
                            ip,
                            mac,
                            vendor: machine.bmc_vendor().into(),
                            standalone: !is_rack_scale_server(machine),
                        });
                    PendingDetails::managed(
                        id.to_string(),
                        endpoint,
                        desired_policy(policies.get(&id).copied()),
                        actions::machine(
                            &machine.state.value,
                            machine.machine_maintenance_requested.as_ref(),
                        ),
                    )
                })
                .collect()
        }
        Target::SwitchIds(list) => {
            let switches = db::switch::find_by(
                txn.as_pgconn(),
                db::ObjectColumnFilter::List(db::switch::IdColumn, &list.ids),
            )
            .await?;
            let managed: HashMap<_, _> = switches
                .into_iter()
                .map(|switch| (switch.id, switch))
                .collect();
            let requests =
                db::switch::find_maintenance_requests(txn.as_pgconn(), &list.ids).await?;
            let rows = db::switch::find_switch_endpoints_by_ids(txn.as_pgconn(), &list.ids).await?;
            let mut endpoints: HashMap<_, _> =
                rows.into_iter().map(|row| (row.switch_id, row)).collect();
            list.ids
                .into_iter()
                .map(|id| {
                    let Some(switch) = managed.get(&id) else {
                        return PendingDetails::missing(id.to_string());
                    };
                    PendingDetails::managed(
                        id.to_string(),
                        endpoints.remove(&id).map(PowerReadEndpoint::Switch),
                        inapplicable_policy(),
                        actions::switch(&switch.controller_state.value, requests.get(&id)),
                    )
                })
                .collect()
        }
        Target::PowerShelfIds(list) => {
            let shelves = db::power_shelf::find_by(
                txn.as_pgconn(),
                db::ObjectColumnFilter::List(db::power_shelf::IdColumn, &list.ids),
            )
            .await?;
            let managed: HashMap<_, _> =
                shelves.into_iter().map(|shelf| (shelf.id, shelf)).collect();
            let requests =
                db::power_shelf::find_maintenance_requests(txn.as_pgconn(), &list.ids).await?;
            let rows =
                db::power_shelf::find_power_shelf_endpoints_by_ids(txn.as_pgconn(), &list.ids)
                    .await?;
            let mut endpoints: HashMap<_, _> = rows
                .into_iter()
                .map(|row| (row.power_shelf_id, row))
                .collect();
            list.ids
                .into_iter()
                .map(|id| {
                    let Some(shelf) = managed.get(&id) else {
                        return PendingDetails::missing(id.to_string());
                    };
                    PendingDetails::managed(
                        id.to_string(),
                        endpoints.remove(&id).map(PowerReadEndpoint::Shelf),
                        inapplicable_policy(),
                        actions::shelf(&shelf.controller_state.value, requests.get(&id)),
                    )
                })
                .collect()
        }
    };
    txn.commit().await?;
    connection.return_to_pool();
    Ok(entries)
}

impl PendingDetails {
    /// Reports unavailable policy/actions for a missing target without a hardware read.
    fn missing(component_id: String) -> Self {
        Self {
            details: rpc::ComponentPowerDetails {
                component_id,
                actual: Some(failed_observation(
                    rpc::ComponentPowerErrorCode::NotFound,
                    None,
                )),
                desired: Some(desired_policy(None)),
                actions: Some(rpc::ComponentPowerActionDetails {
                    actions: vec![],
                    error: Some(rpc::ComponentPowerError {
                        code: rpc::ComponentPowerErrorCode::NotFound as i32,
                        message: "managed component not found".into(),
                    }),
                }),
            },
            endpoint: None,
        }
    }

    /// Preserves saved policy/actions and reports `EndpointUnavailable` until a resolved
    /// endpoint supplies a hardware read.
    fn managed(
        component_id: String,
        endpoint: Option<PowerReadEndpoint>,
        desired: rpc::ComponentPowerPolicy,
        actions: rpc::ComponentPowerActionDetails,
    ) -> Self {
        Self {
            details: rpc::ComponentPowerDetails {
                component_id,
                actual: Some(failed_observation(
                    rpc::ComponentPowerErrorCode::EndpointUnavailable,
                    None,
                )),
                desired: Some(desired),
                actions: Some(actions),
            },
            endpoint,
        }
    }
}

/// Distinguishes absent policy from a stored value, including
/// `PowerManagerDisabled`. Policy versions do not supply a reliable update time.
fn desired_policy(state: Option<model::power_manager::PowerState>) -> rpc::ComponentPowerPolicy {
    let state = state.map(|state| rpc::PowerState::from(state) as i32);
    rpc::ComponentPowerPolicy {
        status: if state.is_some() {
            rpc::ComponentPowerPolicyStatus::Available
        } else {
            rpc::ComponentPowerPolicyStatus::Unavailable
        } as i32,
        source: state.map(|_| rpc::ComponentPowerSource::CorePowerPolicy as i32),
        state,
        updated_at: None,
    }
}

/// Marks managed switches and shelves as having no applicable host power policy.
fn inapplicable_policy() -> rpc::ComponentPowerPolicy {
    rpc::ComponentPowerPolicy {
        status: rpc::ComponentPowerPolicyStatus::NotApplicable as i32,
        ..Default::default()
    }
}

/// `read_actual` selects a passive reader before fetching credentials, then
/// reads one component within the caller's shared deadline.
async fn read_actual(
    api: &Api,
    endpoint: PowerReadEndpoint,
    deadline: tokio::time::Instant,
) -> rpc::ComponentPowerObservation {
    use rpc::ComponentPowerErrorCode as ErrorCode;
    let Some(cm) = api.component_manager.as_ref() else {
        return failed_observation(ErrorCode::Disabled, None);
    };

    // Standalone hosts use direct BMC authentication, including at sites where
    // the general Redfish pool is configured for a proxy. Rack-scale hosts use
    // their configured reader; the configured Core reader also uses this pool.
    let direct_pool: std::sync::Arc<dyn carbide_redfish::libredfish::RedfishClientPool> =
        api.bmc_credential_ops.clone();
    let core = CoreComputeTrayManager::new(direct_pool);
    let compute_reader: &dyn ComputeTrayManager = match &endpoint {
        PowerReadEndpoint::Compute {
            standalone: true, ..
        } => &core,
        _ => cm.compute_tray.as_ref(),
    };
    let source = match &endpoint {
        PowerReadEndpoint::Compute { .. } => compute_reader.power_state_source(),
        PowerReadEndpoint::Switch(_) => cm.nv_switch.power_state_source(),
        PowerReadEndpoint::Shelf(_) => cm.power_shelf.power_state_source(),
    };
    let Some(source) = source else {
        return failed_observation(ErrorCode::Unsupported, None);
    };
    let source = match source {
        PowerStateSource::Redfish => rpc::ComponentPowerSource::Redfish,
        PowerStateSource::Rms => rpc::ComponentPowerSource::Rms,
    };
    let mac = match &endpoint {
        PowerReadEndpoint::Compute { mac, .. } => *mac,
        PowerReadEndpoint::Switch(row) => row.bmc_mac,
        PowerReadEndpoint::Shelf(row) => row.pmc_mac,
    };

    let credentials = match &endpoint {
        PowerReadEndpoint::Compute { .. } => {
            fetch_compute_tray_bmc_credentials(api.credential_manager.as_ref(), mac).await
        }
        PowerReadEndpoint::Switch(_) => {
            fetch_switch_bmc_credentials(api.credential_manager.as_ref(), mac).await
        }
        PowerReadEndpoint::Shelf(_) => {
            fetch_powershelf_pmc_credentials(api.credential_manager.as_ref(), mac).await
        }
    };
    // The caller's `timeout_at` polls ready results before checking expiry.
    // Check here so a late error cannot replace `DeadlineExceeded` and late
    // credentials cannot start another read.
    if tokio::time::Instant::now() >= deadline {
        return failed_observation(ErrorCode::DeadlineExceeded, Some(source));
    }
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::warn!(
                management_mac_address = %mac,
                ?source,
                error_category = backend_error_category(&error),
                "power read management credentials are unavailable"
            );
            return failed_observation(ErrorCode::CredentialsUnavailable, Some(source));
        }
    };

    let result = match endpoint {
        PowerReadEndpoint::Compute {
            ip, mac, vendor, ..
        } => {
            let endpoint = ComputeTrayEndpoint {
                bmc_ip: ip,
                bmc_mac: mac,
                vendor,
                bmc_credentials: credentials,
            };
            compute_reader.get_power_state(&[endpoint]).await
        }
        PowerReadEndpoint::Switch(row) => {
            let (Some(nvos_mac), Some(nvos_ip)) = (row.nvos_mac, row.nvos_ip) else {
                return failed_observation(ErrorCode::EndpointUnavailable, Some(source));
            };
            let nvos_credentials =
                fetch_switch_nvos_credentials(api.credential_manager.as_ref(), mac).await;
            // Apply the same guard before classifying the second credential lookup.
            if tokio::time::Instant::now() >= deadline {
                return failed_observation(ErrorCode::DeadlineExceeded, Some(source));
            }
            let nvos_credentials = match nvos_credentials {
                Ok(credentials) => credentials,
                Err(error) => {
                    tracing::warn!(
                        management_mac_address = %mac,
                        ?source,
                        error_category = backend_error_category(&error),
                        "power read NVOS credentials are unavailable"
                    );
                    return failed_observation(ErrorCode::CredentialsUnavailable, Some(source));
                }
            };
            let endpoint = SwitchEndpoint {
                bmc_ip: row.bmc_ip,
                bmc_mac: mac,
                nvos_ip,
                nvos_mac,
                bmc_credentials: credentials,
                nvos_credentials,
                nvos_host_name: row.nvos_hostname,
            };
            cm.nv_switch.get_power_state(&[endpoint]).await
        }
        PowerReadEndpoint::Shelf(row) => {
            let endpoint = PowerShelfEndpoint {
                pmc_ip: row.pmc_ip,
                pmc_mac: mac,
                pmc_vendor: PowerShelfVendor::DEFAULT,
                pmc_credentials: credentials,
            };
            cm.power_shelf.get_power_state(&[endpoint]).await
        }
    };
    // Reject expired results before `observation` classifies failures or sets
    // `observed_at` on a successful read.
    if tokio::time::Instant::now() >= deadline {
        return failed_observation(ErrorCode::DeadlineExceeded, Some(source));
    }
    observation(result, mac, source)
}

/// Accepts exactly one result with the requested MAC and omits all backend text
/// from wire errors and diagnostics. Explicit Unknown remains an observation.
fn observation(
    result: Result<Vec<ComponentPowerStateResult>, ComponentManagerError>,
    mac: MacAddress,
    source: rpc::ComponentPowerSource,
) -> rpc::ComponentPowerObservation {
    use rpc::ComponentPowerErrorCode as ErrorCode;
    let state = match result {
        Err(ComponentManagerError::Unsupported(_)) => {
            tracing::debug!(
                management_mac_address = %mac,
                ?source,
                error_category = "unsupported",
                "passive power read is unsupported"
            );
            return failed_observation(ErrorCode::Unsupported, Some(source));
        }
        Err(error) => {
            tracing::warn!(
                management_mac_address = %mac,
                ?source,
                error_category = backend_error_category(&error),
                "component power read failed"
            );
            return failed_observation(ErrorCode::ReadFailed, Some(source));
        }
        Ok(results) => match results.as_slice() {
            [result] if result.mac_address == mac => match result.power_state {
                Ok(Some(state)) => state,
                Ok(None) => return failed_observation(ErrorCode::NoObservation, Some(source)),
                Err(_) => {
                    tracing::warn!(
                        management_mac_address = %mac,
                        ?source,
                        result_count = 1,
                        error_category = "per_target_failure",
                        "backend reported a component power read failure"
                    );
                    return failed_observation(ErrorCode::ReadFailed, Some(source));
                }
            },
            [] => return failed_observation(ErrorCode::NoObservation, Some(source)),
            _ => {
                tracing::warn!(
                    management_mac_address = %mac,
                    ?source,
                    result_count = results.len(),
                    "component power results do not match the requested identity and count"
                );
                return failed_observation(ErrorCode::ReadFailed, Some(source));
            }
        },
    };
    let state = match state {
        PowerState::On => rpc::ComponentPowerState::On,
        PowerState::Off => rpc::ComponentPowerState::Off,
        PowerState::PoweringOn => rpc::ComponentPowerState::PoweringOn,
        PowerState::PoweringOff => rpc::ComponentPowerState::PoweringOff,
        PowerState::Paused => rpc::ComponentPowerState::Paused,
        PowerState::Reset => rpc::ComponentPowerState::Reset,
        PowerState::Unknown => rpc::ComponentPowerState::Unknown,
    };
    rpc::ComponentPowerObservation {
        outcome: Some(PowerOutcome::State(state as i32)),
        source: Some(source as i32),
        observed_at: Some(chrono::Utc::now().into()),
    }
}

/// Classifies whole-call failures without exposing arbitrary backend payloads.
fn backend_error_category(error: &ComponentManagerError) -> &'static str {
    match error {
        ComponentManagerError::Unavailable(_) => "unavailable",
        ComponentManagerError::NotFound(_) => "not_found",
        ComponentManagerError::InvalidArgument(_) => "invalid_argument",
        ComponentManagerError::Unsupported(_) => "unsupported",
        ComponentManagerError::RejectedBeforeDispatch(_) => "rejected_before_dispatch",
        ComponentManagerError::OperationOutcomeUnknown(_) => "operation_outcome_unknown",
        ComponentManagerError::Internal(_) => "internal",
        ComponentManagerError::Transport(_) => "transport",
        ComponentManagerError::Status(_) => "grpc_status",
        ComponentManagerError::Rms(_) => "rms",
    }
}

/// Builds a categorized failure from fixed text, with no state or receipt time.
fn failed_observation(
    code: rpc::ComponentPowerErrorCode,
    source: Option<rpc::ComponentPowerSource>,
) -> rpc::ComponentPowerObservation {
    use rpc::ComponentPowerErrorCode as ErrorCode;
    let message = match code {
        ErrorCode::NotFound => "managed component not found",
        ErrorCode::Disabled => "component manager is not configured",
        ErrorCode::Unsupported => "backend does not support passive live power reads",
        ErrorCode::EndpointUnavailable => "management endpoint is unavailable",
        ErrorCode::CredentialsUnavailable => "management credentials are unavailable",
        ErrorCode::NoObservation => "backend supplied no power observation",
        ErrorCode::DeadlineExceeded => "power read deadline exceeded",
        ErrorCode::ReadFailed | ErrorCode::Unspecified => "backend power read failed",
    };
    rpc::ComponentPowerObservation {
        source: source.map(|source| source as i32),
        observed_at: None,
        outcome: Some(PowerOutcome::Error(rpc::ComponentPowerError {
            code: code as i32,
            message: message.into(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
    use carbide_uuid::power_shelf::PowerShelfId;
    use carbide_uuid::switch::SwitchId;

    use super::*;

    /// Snapshot setup must configure read-only, repeatable-read queries and the
    /// server cap without extending a tighter operator limit or changing the
    /// setting used by later pool borrowers.
    #[crate::sqlx_test]
    async fn snapshot_statement_timeout_is_capped_and_transaction_local(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = DbSessionGuard::acquire(&pool).await?;
        for (configured_ms, expected_ms) in [
            // A disabled server timeout still needs the snapshot's limit.
            (0, 5000_i64),
            // A broader site limit must not extend snapshot SQL work.
            (60000, 5000),
            // Preserve an operator's stricter limit.
            (1000, 1000),
        ] {
            sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                .bind(configured_ms.to_string())
                .execute(
                    connection
                        .connection
                        .as_deref_mut()
                        .expect("snapshot lease"),
                )
                .await?;

            let mut txn = connection.begin().await?;
            let effective_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(txn.as_pgconn())
            .await?;
            assert_eq!(effective_ms, expected_ms, "configured {configured_ms}ms");
            let (read_only, isolation): (String, String) = sqlx::query_as(
                "SELECT current_setting('transaction_read_only'), \
                        current_setting('transaction_isolation')",
            )
            .fetch_one(txn.as_pgconn())
            .await?;
            assert_eq!(read_only, "on");
            assert_eq!(isolation, "repeatable read");
            txn.commit().await?;

            let restored_ms: i64 = sqlx::query_scalar(
                "SELECT setting::bigint FROM pg_settings WHERE name = 'statement_timeout'",
            )
            .fetch_one(
                connection
                    .connection
                    .as_deref_mut()
                    .expect("snapshot lease"),
            )
            .await?;
            assert_eq!(restored_ms, configured_ms, "configured {configured_ms}ms");
        }
        connection.return_to_pool();

        Ok(())
    }

    /// Keep invalid batches out of snapshot acquisition while preserving each
    /// typed target arm and the inclusive request-size boundary.
    #[test]
    fn validates_bounded_unique_targets() {
        let switches = |count| {
            Target::SwitchIds(rpc::SwitchIdList {
                ids: (1..=count)
                    .map(|index| SwitchId::from(uuid::Uuid::from_u128(index)))
                    .collect(),
            })
        };
        let host = HostMachineId::try_from(MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [1; 32],
            MachineType::Host,
        ))
        .expect("host ID fixture");
        let shelf = PowerShelfId::from(uuid::Uuid::from_u128(1));
        check_cases(
            [
                Case {
                    scenario: "empty batch",
                    input: switches(0),
                    expect: FailsWith(tonic::Code::InvalidArgument),
                },
                Case {
                    scenario: "maximum batch",
                    input: switches(64),
                    expect: Yields(()),
                },
                Case {
                    scenario: "oversized batch",
                    input: switches(65),
                    expect: FailsWith(tonic::Code::InvalidArgument),
                },
                Case {
                    scenario: "duplicate shelves",
                    input: Target::PowerShelfIds(rpc::PowerShelfIdList {
                        ids: vec![shelf, shelf],
                    }),
                    expect: FailsWith(tonic::Code::InvalidArgument),
                },
                Case {
                    scenario: "singleton host",
                    input: Target::MachineIds(::rpc::common::HostMachineIdList {
                        machine_ids: vec![host],
                    }),
                    expect: Yields(()),
                },
            ],
            |target| validate_target(&target).map_err(|error| Status::from(error).code()),
        );
    }

    /// Only one matching observation may become actual state with a receipt
    /// time from this conversion. Malformed or failed evidence must select the
    /// error outcome without leaking backend payloads.
    #[test]
    fn observations_preserve_unknown_and_reject_missing_or_mismatched_evidence() {
        use self::rpc::ComponentPowerErrorCode as ErrorCode;

        let mac = MacAddress::new([1, 2, 3, 4, 5, 6]);
        let single = |mac_address, power_state| {
            Ok(vec![ComponentPowerStateResult {
                mac_address,
                power_state,
            }])
        };

        // Cover wire states absent from the public acceptance fixtures here;
        // rejected evidence shares the same projection and redaction contract.
        check_values(
            [
                Check {
                    scenario: "explicit unknown",
                    input: single(mac, Ok(Some(PowerState::Unknown))),
                    expect: Ok(rpc::ComponentPowerState::Unknown),
                },
                Check {
                    scenario: "transition toward power off",
                    input: single(mac, Ok(Some(PowerState::PoweringOff))),
                    expect: Ok(rpc::ComponentPowerState::PoweringOff),
                },
                Check {
                    scenario: "paused hardware",
                    input: single(mac, Ok(Some(PowerState::Paused))),
                    expect: Ok(rpc::ComponentPowerState::Paused),
                },
                Check {
                    scenario: "reset hardware",
                    input: single(mac, Ok(Some(PowerState::Reset))),
                    expect: Ok(rpc::ComponentPowerState::Reset),
                },
                Check {
                    scenario: "no state",
                    input: single(mac, Ok(None)),
                    expect: Err(ErrorCode::NoObservation),
                },
                Check {
                    scenario: "per-target failure hides private payload",
                    input: single(mac, Err("private payload".into())),
                    expect: Err(ErrorCode::ReadFailed),
                },
                Check {
                    scenario: "wrong identity",
                    input: single(
                        MacAddress::new([6, 5, 4, 3, 2, 1]),
                        Ok(Some(PowerState::On)),
                    ),
                    expect: Err(ErrorCode::ReadFailed),
                },
                Check {
                    scenario: "whole-call unsupported hides private payload",
                    input: Err(ComponentManagerError::Unsupported("private payload".into())),
                    expect: Err(ErrorCode::Unsupported),
                },
                Check {
                    scenario: "whole-call failure hides private payload",
                    input: Err(ComponentManagerError::Internal("private payload".into())),
                    expect: Err(ErrorCode::ReadFailed),
                },
                Check {
                    scenario: "empty result",
                    input: Ok(vec![]),
                    expect: Err(ErrorCode::NoObservation),
                },
                Check {
                    scenario: "duplicate matching identities",
                    input: Ok(vec![
                        ComponentPowerStateResult {
                            mac_address: mac,
                            power_state: Ok(Some(PowerState::On)),
                        };
                        2
                    ]),
                    expect: Err(ErrorCode::ReadFailed),
                },
            ],
            |result| {
                // The receipt timestamp must describe this conversion, not a
                // default timestamp or a value cached by the backend.
                let before = chrono::Utc::now();
                let actual = observation(result, mac, rpc::ComponentPowerSource::Rms);
                let after = chrono::Utc::now();

                assert_eq!(actual.source, Some(rpc::ComponentPowerSource::Rms as i32));
                assert!(!format!("{actual:?}").contains("private payload"));
                match actual.outcome.expect("every observation has an outcome") {
                    PowerOutcome::State(state) => {
                        let observed_at: chrono::DateTime<chrono::Utc> = actual
                            .observed_at
                            .expect("successful observations have a receipt time")
                            .try_into()
                            .expect("receipt time is a valid UTC timestamp");
                        assert!((before..=after).contains(&observed_at));
                        Ok(rpc::ComponentPowerState::try_from(state).unwrap())
                    }
                    PowerOutcome::Error(error) => {
                        assert!(actual.observed_at.is_none());
                        Err(ErrorCode::try_from(error.code).unwrap())
                    }
                }
            },
        );
    }
}

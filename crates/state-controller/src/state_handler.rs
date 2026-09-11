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
use std::panic::Location;

use carbide_uuid::machine::{InvalidMachineType, MachineId};
use db::DatabaseError;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::ManagedHostState;
use model::resource_pool::ResourcePoolError;
use sqlx::{PgPool, PgTransaction};

use crate::db_write_batch::DbWriteBatch;

/// The collection of generic objects which are referenced in StateHandlerContext
pub trait StateHandlerContextObjects: Send + Sync + 'static {
    /// The type of services accessible on the state handler context object
    /// via [`StateHandlerContext::services`]
    type Services: Clone + Send + Sync + 'static;

    /// The type that can hold metrics specific to a single object.
    ///
    /// These metrics can be produced by code inside the state handler by writing
    /// them to `ObjectMetrics`.
    /// After state has been processed for all all objects, the various metrics
    /// are merged into an `IterationMetrics` object.
    type ObjectMetrics: std::fmt::Debug + Default + Send + Sync + 'static;
}

/// Context parameter passed to `StateHandler`
pub struct StateHandlerContext<'a, T: StateHandlerContextObjects> {
    /// Services that are available to the `StateHandler`
    pub services: &'a mut T::Services,
    /// Metrics that are produced as a result of acting on an object
    pub metrics: &'a mut T::ObjectMetrics,
    pub pending_db_writes: &'a mut DbWriteBatch,
}

/// Defines a function that will be called to determine the next step in
/// an objects lifecycle.
///
/// The function retrieves the full Object state as loaded from the database
/// as input, and can take any decisions to advance the Object state.
#[async_trait::async_trait]
pub trait StateHandler: std::fmt::Debug + Send + Sync + 'static {
    type ObjectId: Clone + std::fmt::Display + std::fmt::Debug;
    type State;
    type ControllerState;
    type ContextObjects: StateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &Self::ObjectId,
        state: &mut Self::State,
        controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError>;
}

#[derive(Debug)]
pub struct ExternalServiceError {
    service: &'static str,
    operation: &'static str,
    details: String,
    metric_label: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl ExternalServiceError {
    pub fn new(
        service: &'static str,
        operation: &'static str,
        details: impl Into<String>,
        metric_label: &'static str,
    ) -> Self {
        Self {
            service,
            operation,
            details: details.into(),
            metric_label,
            source: None,
        }
    }

    pub fn with_source(
        service: &'static str,
        operation: &'static str,
        details: impl Into<String>,
        metric_label: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            service,
            operation,
            details: details.into(),
            metric_label,
            source: Some(Box::new(source)),
        }
    }

    pub fn service(&self) -> &'static str {
        self.service
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn metric_label(&self) -> &'static str {
        self.metric_label
    }
}

impl std::fmt::Display for ExternalServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.operation.is_empty() {
            write!(
                f,
                "Failed {} operation. Details: {}.",
                self.service, self.details
            )
        } else {
            write!(
                f,
                "Failed {} operation: {}. Details: {}.",
                self.service, self.operation, self.details
            )
        }
    }
}

impl std::error::Error for ExternalServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn std::error::Error + 'static))
    }
}

pub enum StateHandlerOutcome<S> {
    Wait {
        /// The reason we're waiting
        reason: String,
        source_ref: &'static Location<'static>,
        txn: Option<PgTransaction<'static>>,
    },
    Transition {
        /// The state we are transitioning to
        next_state: S,
        source_ref: &'static Location<'static>,
        txn: Option<PgTransaction<'static>>,
    },
    DoNothing {
        source_ref: &'static Location<'static>,
        txn: Option<PgTransaction<'static>>,
    }, // Nothing to do. Typically in Ready or Assigned/Ready
    Deleted {
        _source_ref: &'static Location<'static>,
        txn: Option<PgTransaction<'static>>,
    }, // The object was removed from the database
}

impl<S> StateHandlerOutcome<S> {
    pub fn with_txn(self, transaction: PgTransaction<'static>) -> StateHandlerOutcome<S> {
        self.with_txn_opt(Some(transaction))
    }

    pub fn with_txn_opt(
        mut self,
        transaction: Option<PgTransaction<'static>>,
    ) -> StateHandlerOutcome<S> {
        debug_assert!(
            self.take_transaction().is_none(),
            "BUG: with_txn called on a StateHandlerOutcome that already has a transaction!"
        );
        match self {
            Self::Wait {
                reason,
                source_ref,
                txn: _,
            } => Self::Wait {
                reason,
                source_ref,
                txn: transaction,
            },
            Self::Transition {
                next_state,
                source_ref,
                txn: _,
            } => Self::Transition {
                next_state,
                source_ref,
                txn: transaction,
            },
            Self::DoNothing { source_ref, txn: _ } => Self::DoNothing {
                source_ref,
                txn: transaction,
            },
            Self::Deleted {
                _source_ref,
                txn: _,
            } => Self::Deleted {
                _source_ref,
                txn: transaction,
            },
        }
    }

    #[track_caller]
    pub fn do_nothing() -> Self {
        StateHandlerOutcome::DoNothing {
            source_ref: Location::caller(),
            txn: None,
        }
    }

    #[track_caller]
    pub fn transition(next_state: S) -> Self {
        StateHandlerOutcome::Transition {
            next_state,
            source_ref: Location::caller(),
            txn: None,
        }
    }

    #[track_caller]
    pub fn wait(reason: String) -> Self {
        StateHandlerOutcome::Wait {
            reason,
            source_ref: Location::caller(),
            txn: None,
        }
    }

    #[track_caller]
    pub fn deleted() -> Self {
        StateHandlerOutcome::Deleted {
            _source_ref: Location::caller(),
            txn: None,
        }
    }

    pub fn take_transaction(&mut self) -> Option<PgTransaction<'static>> {
        match self {
            StateHandlerOutcome::Wait { txn, .. } => txn,
            StateHandlerOutcome::Transition { txn, .. } => txn,
            StateHandlerOutcome::DoNothing { txn, .. } => txn,
            StateHandlerOutcome::Deleted { txn, .. } => txn,
        }
        .take()
    }

    /// Ensures this StateHandlerOutcome contains a PgTransaction (starting a new one if not) then
    /// calls the passed async closure with it. If successful, returns self.
    pub async fn in_transaction<'a, E>(
        mut self,
        pg_pool: &'a PgPool,
        f: impl for<'txn> FnOnce(
            &'txn mut PgTransaction<'static>,
        ) -> futures::future::BoxFuture<'txn, Result<(), E>>
        + Send,
    ) -> sqlx::Result<Result<Self, E>>
    where
        E: Send,
    {
        let txn_opt = match &mut self {
            StateHandlerOutcome::Wait { txn, .. } => txn,
            StateHandlerOutcome::Transition { txn, .. } => txn,
            StateHandlerOutcome::DoNothing { txn, .. } => txn,
            StateHandlerOutcome::Deleted { txn, .. } => txn,
        };

        let txn = match txn_opt {
            Some(txn) => txn,
            None => txn_opt.insert(pg_pool.begin().await?),
        };

        Ok(f(txn).await.map(|()| self))
    }
}

impl<S> std::fmt::Display for StateHandlerOutcome<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        use StateHandlerOutcome::*;
        let msg = match self {
            Wait { reason, .. } => reason.as_str(),
            Transition { .. } => "Transition to next state",
            DoNothing { .. } => "Do nothing",
            Deleted { .. } => "Deleted",
        };
        write!(f, "{msg}")
    }
}

/// Error type for handling a Machine State
#[derive(Debug, thiserror::Error)]
pub enum StateHandlerError {
    #[error("unable to perform database transaction: {0}")]
    TransactionError(#[source] Box<sqlx::Error>),
    #[error("failed to advance state: {0}")]
    GenericError(eyre::Report),
    #[error("state for object {object_id} can not be advanced. missing data: {missing}")]
    MissingData {
        object_id: String,
        missing: &'static str,
    },
    #[error("{0}")]
    DBError(#[source] Box<DatabaseError>),

    #[error("error releasing from resource pool: {0}")]
    PoolReleaseError(#[source] Box<ResourcePoolError>),

    #[error("invalid host state {1} for machine {0}")]
    InvalidHostState(MachineId, Box<ManagedHostState>),

    #[error(transparent)]
    ExternalServiceError(#[from] ExternalServiceError),

    #[error("the state handler for object {object_id} in state \"{state}\" timed out")]
    Timeout { object_id: String, state: String },

    #[error("failed to update firmware: {0}")]
    FirmwareUpdateError(eyre::Report),

    #[error("manual intervention required. cannot make progress. {0}")]
    ManualInterventionRequired(String),

    #[error("invalid state: {0}")]
    InvalidState(String),

    #[error("state will not be advanced due to health probe alert")]
    HealthProbeAlert,

    #[error(
        "the object is in the state for longer than defined by the SLA. handler outcome: {handler_outcome}"
    )]
    TimeInStateAboveSla { handler_outcome: String },

    #[error("resource {resource} cleanup error: {error}")]
    ResourceCleanupError {
        resource: &'static str,
        error: String,
    },

    #[error("spdm error: {0}")]
    SpdmError(#[source] Box<model::attestation::spdm::SpdmHandlerError>),

    // This error is temporary while parts of the codebase migrate to using HostMachineId for places
    // where only predicted and stable host ID's are acceptable. Any time it is raised is a bug in
    // the code, so report the caller
    #[error("bug: invalid machine ID at {location}: {error}")]
    InvalidHostMachineId {
        location: &'static std::panic::Location<'static>,
        error: InvalidMachineType,
    },
}

impl From<InvalidMachineType> for StateHandlerError {
    #[track_caller]
    fn from(value: InvalidMachineType) -> Self {
        let location = std::panic::Location::caller();
        Self::InvalidHostMachineId {
            location,
            error: value,
        }
    }
}

impl StateHandlerError {
    /// Returns the label that will be used to identify the error in metrics
    ///
    /// This will be a simplified description of the error, to avoid having too
    /// many metric dimensions.
    pub fn metric_label(&self) -> &'static str {
        match self {
            StateHandlerError::TransactionError(_) => "transaction_error",
            StateHandlerError::GenericError(_) => "generic_error",
            StateHandlerError::FirmwareUpdateError(_) => "firware_update_error",
            StateHandlerError::MissingData { .. } => "missing_data",
            StateHandlerError::DBError(_) => "db_error",
            StateHandlerError::Timeout { .. } => "timeout",
            StateHandlerError::PoolReleaseError(_) => "pool_release_error",
            StateHandlerError::InvalidHostState(_, _) => "invalid_host_state",
            StateHandlerError::InvalidState(_) => "invalid_state",
            StateHandlerError::ExternalServiceError(error) => error.metric_label(),
            StateHandlerError::ManualInterventionRequired(_) => "manual_intervention_required",
            StateHandlerError::HealthProbeAlert => "health_probe_alert",
            StateHandlerError::TimeInStateAboveSla { .. } => "time_in_state_above_sla",
            StateHandlerError::ResourceCleanupError { resource, .. } => match *resource {
                "VpcLoopbackIp" => "vpcloopback_release_failed",
                "network_segment" => "network_segment_cleanup_failed",
                _ => "resource_cleanup_failed",
            },
            StateHandlerError::SpdmError(_) => "spdm_attestation_error",
            StateHandlerError::InvalidHostMachineId { .. } => "invalid_host_machine_id",
        }
    }
}

impl From<sqlx::Error> for StateHandlerError {
    fn from(error: sqlx::Error) -> Self {
        Self::TransactionError(Box::new(error))
    }
}

impl From<DatabaseError> for StateHandlerError {
    fn from(error: DatabaseError) -> Self {
        Self::DBError(Box::new(error))
    }
}

impl From<ResourcePoolError> for StateHandlerError {
    fn from(error: ResourcePoolError) -> Self {
        Self::PoolReleaseError(Box::new(error))
    }
}

impl From<model::attestation::spdm::SpdmHandlerError> for StateHandlerError {
    fn from(error: model::attestation::spdm::SpdmHandlerError) -> Self {
        Self::SpdmError(Box::new(error))
    }
}

/// A `StateHandler` implementation which does nothing
pub struct NoopStateHandler<I, S, CS, CO> {
    _phantom_data: std::marker::PhantomData<Option<(I, S, CS, CO)>>,
}

impl<I, S, CS, CO> std::fmt::Debug for NoopStateHandler<I, S, CS, CO> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoopStateHandler").finish()
    }
}

impl<I, S, CS, CO> Default for NoopStateHandler<I, S, CS, CO> {
    fn default() -> Self {
        Self {
            _phantom_data: Default::default(),
        }
    }
}

#[async_trait::async_trait]
impl<
    I: Clone + std::fmt::Display + std::fmt::Debug + Send + Sync + 'static,
    S: Send + Sync + 'static,
    CS: Send + Sync + 'static,
    CO: StateHandlerContextObjects,
> StateHandler for NoopStateHandler<I, S, CS, CO>
{
    type State = S;
    type ControllerState = CS;
    type ObjectId = I;
    type ContextObjects = CO;

    async fn handle_object_state(
        &self,
        _object_id: &Self::ObjectId,
        _state: &mut Self::State,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        Ok(StateHandlerOutcome::do_nothing())
    }
}

pub trait FromStateHandlerResult<S> {
    fn from_result(r: Result<&StateHandlerOutcome<S>, &StateHandlerError>) -> Self;
}

impl<S> FromStateHandlerResult<S> for PersistentStateHandlerOutcome {
    fn from_result(
        r: Result<&StateHandlerOutcome<S>, &StateHandlerError>,
    ) -> PersistentStateHandlerOutcome {
        match r {
            Ok(StateHandlerOutcome::Wait {
                reason, source_ref, ..
            }) => PersistentStateHandlerOutcome::Wait {
                reason: reason.clone(),
                source_ref: Some(source_ref.into()),
            },
            Ok(StateHandlerOutcome::Transition { source_ref, .. }) => {
                PersistentStateHandlerOutcome::Transition {
                    source_ref: Some(source_ref.into()),
                }
            }
            Ok(StateHandlerOutcome::DoNothing { source_ref, .. }) => {
                PersistentStateHandlerOutcome::DoNothing {
                    source_ref: Some(source_ref.into()),
                }
            }
            Ok(StateHandlerOutcome::Deleted { .. }) => unreachable!(),
            Err(err) => PersistentStateHandlerOutcome::Error {
                err: err.to_string(),
                // TODO: Make it possible to determine where errors are generated
                source_ref: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_transition_source_location() {
        let StateHandlerOutcome::<String>::DoNothing { source_ref, .. } =
            StateHandlerOutcome::do_nothing()
        else {
            unreachable!()
        };
        assert_eq!(source_ref.line(), line!() - 4);
        assert_eq!(source_ref.file(), file!());

        let StateHandlerOutcome::<String>::Wait { source_ref, .. } =
            StateHandlerOutcome::wait("reason".into())
        else {
            unreachable!()
        };
        assert_eq!(source_ref.line(), line!() - 4);
        assert_eq!(source_ref.file(), file!());

        let StateHandlerOutcome::<String>::Transition { source_ref, .. } =
            StateHandlerOutcome::transition("next".into())
        else {
            unreachable!()
        };
        assert_eq!(source_ref.line(), line!() - 4);
        assert_eq!(source_ref.file(), file!());

        let StateHandlerOutcome::<String>::Deleted { _source_ref, .. } =
            StateHandlerOutcome::deleted()
        else {
            unreachable!()
        };
        assert_eq!(_source_ref.line(), line!() - 4);
        assert_eq!(_source_ref.file(), file!());
    }
}

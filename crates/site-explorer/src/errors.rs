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
use carbide_resource_pool::registration::RegistrationError;
use carbide_resource_pool::remote::RemoteResourcePoolError;
use carbide_resource_pool::{ResourcePoolBackendError, ResourcePoolRegistryError};
use carbide_uuid::machine::InvalidMachineType;
use db::DatabaseError;
use db::work_lock_manager::AcquireLockError;
use model::errors::ModelError;
use model::site_explorer::EndpointExplorationError;

#[derive(thiserror::Error, Debug)]
pub enum SiteExplorerError {
    /// DPU loopback selection, allocation, or cleanup could not finish.
    #[error(transparent)]
    ResourcePool(Box<ResourcePoolBackendError>),
    #[error("database error: {0}")]
    DatabaseError(#[from] DatabaseError),
    #[error("model error: {0}")]
    ModelError(#[from] ModelError),
    #[error("{kind} already exists: {id}")]
    AlreadyFoundError {
        /// The type of the resource that already exists (e.g. Machine)
        kind: &'static str,
        /// The ID of the resource that already exists.
        id: String,
    },
    #[error("{kind} not found: {id}")]
    NotFoundError {
        /// The type of the resource that was not found (e.g. Machine)
        kind: &'static str,
        /// The ID of the resource that was not found
        id: String,
    },
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    #[error("EndpointExplorationError for {action}: {err}")]
    EndpointExplorationError {
        action: &'static str,
        /// Actual endpiing exploration error.
        err: EndpointExplorationError,
    },
    #[error("internal error: {message}")]
    Internal { message: String },

    // This error is temporary while parts of the codebase migrate to using HostMachineId for places
    // where only predicted and stable host ID's are acceptable. Any time it is raised is a bug in
    // the code, so report the caller
    #[error("bug: invalid machine ID at {location}: {error}")]
    InvalidHostMachineId {
        location: &'static std::panic::Location<'static>,
        error: InvalidMachineType,
    },
}

impl From<db::machine::DpuHostAssociationError> for SiteExplorerError {
    fn from(error: db::machine::DpuHostAssociationError) -> Self {
        Self::internal(error.to_string())
    }
}

impl From<ResourcePoolBackendError> for SiteExplorerError {
    /// Keep database failures in the same category as other Site Explorer writes.
    fn from(error: ResourcePoolBackendError) -> Self {
        match error {
            ResourcePoolBackendError::Database(error)
            | ResourcePoolBackendError::Remote(RemoteResourcePoolError::Database(error))
            | ResourcePoolBackendError::Remote(RemoteResourcePoolError::Acquire(
                AcquireLockError::Database(error),
            ))
            | ResourcePoolBackendError::Registry(ResourcePoolRegistryError::Database(error))
            | ResourcePoolBackendError::Registration(RegistrationError::Database(error)) => {
                Self::DatabaseError(error)
            }
            ResourcePoolBackendError::Integrated(
                db::resource_pool::ResourcePoolDatabaseError::Database(error),
            ) => Self::DatabaseError(*error),
            error => Self::ResourcePool(Box::new(error)),
        }
    }
}

impl From<InvalidMachineType> for SiteExplorerError {
    #[track_caller]
    fn from(err: InvalidMachineType) -> Self {
        Self::invalid_host_machine_id(err)
    }
}

impl SiteExplorerError {
    /// Creates a `Internal` error with the given error message
    pub fn internal(message: String) -> Self {
        Self::Internal { message }
    }

    #[track_caller]
    pub fn invalid_host_machine_id(error: InvalidMachineType) -> Self {
        let location = std::panic::Location::caller();
        Self::InvalidHostMachineId { location, error }
    }
}

pub type SiteExplorerResult<T> = Result<T, SiteExplorerError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed work-lock write is still a database failure after it passes
    /// through the remote allocator and Site Explorer's error conversion.
    #[test]
    fn resource_pool_work_lock_database_error_keeps_its_category() {
        let database_error = DatabaseError::query("work-lock insert", sqlx::Error::PoolClosed);
        let error = SiteExplorerError::from(ResourcePoolBackendError::Remote(
            RemoteResourcePoolError::Acquire(AcquireLockError::Database(database_error)),
        ));
        assert!(matches!(error, SiteExplorerError::DatabaseError(_)));
    }
}

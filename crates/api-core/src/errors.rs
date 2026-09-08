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
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::panic::Location;
use std::sync::Arc;

use ::rpc::errors::RpcDataConversionError;
use carbide_ib_fabric::errors::IbError;
use carbide_redfish::libredfish::RedfishClientCreationError;
use carbide_redfish::libredfish::dpu_bios::is_dpu_bios_attributes_not_ready;
use carbide_site_explorer::EndpointExplorationServiceError;
use carbide_uuid::machine::{InvalidMachineType, MachineId};
use config_version::ConfigVersionParseError;
use db::credential_rotation::CredentialRotationType;
use db::ip_allocator::DhcpError;
use db::machine_interface_address::AddressAlreadyInUseError;
use db::resource_pool::ResourcePoolDatabaseError;
use db::{AnnotatedSqlxError, DatabaseError};
use librms::RackManagerError;
use mac_address::MacAddress;
use model::errors::{ErrorCode, ErrorSubsystem, ModelError, OperatorError};
use model::hardware_info::HardwareInfoError;
use model::network_devices::LldpError;
use model::site_explorer::EndpointExplorationError;
use model::tenant::TenantError;
use model::vpc::VpcCapabilityError;
use model::{ConfigValidationError, resource_pool};
use tonic::Status;
use tonic::metadata::MetadataValue;

/// Represents various Errors that can occur throughout the system.
///
/// CarbideError is a way to represent and enrich lower-level errors with specific business logic
/// that can be handled.
///
/// It uses `thiserror` to adapt lower-level errors to this type.
#[derive(thiserror::Error, Debug)]
pub enum CarbideError {
    #[error("generic error from report: {0}")]
    GenericErrorFromReport(#[from] eyre::ErrReport),

    #[error("unable to parse string into IP network: {0}")]
    NetworkParseError(#[from] ipnetwork::IpNetworkError),

    #[error("unable to parse string into IP address: {0}")]
    AddressParseError(#[from] std::net::AddrParseError),

    #[error("unable to parse string into mac address: {0}")]
    MacAddressParseError(#[from] mac_address::MacParseError),

    #[error("uuid type conversion error: {0}")]
    UuidConversionError(#[from] uuid::Error),

    #[error("RPC uuid type conversion error: {0}")]
    RpcUuidConversionError(#[from] carbide_uuid::UuidConversionError),

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

    #[error("argument is missing in input: {0}")]
    MissingArgument(&'static str),

    #[error("argument is invalid: {0}")]
    InvalidArgument(String),

    #[error("argument is invalid: {0}")]
    VpcCapability(#[from] VpcCapabilityError),

    #[error(transparent)]
    AddressAlreadyInUse(#[from] AddressAlreadyInUseError),

    #[error("{0}")]
    DBError(#[from] AnnotatedSqlxError),

    #[error("database type conversion error")]
    DatabaseTypeConversionError(String),

    #[error("database migration error: {0}")]
    DatabaseMigrationError(#[from] sqlx::migrate::MigrateError),

    #[error("duplicate MAC address for network: {0}")]
    NetworkSegmentDuplicateMacAddress(MacAddress),

    #[error("duplicate MAC address for expected host BMC interface: {0}")]
    ExpectedHostDuplicateMacAddress(MacAddress),

    #[error("NVOS MAC address is already claimed by another expected switch: {0}")]
    ExpectedSwitchDuplicateNvosMacAddress(MacAddress),

    #[error("admin network is not configured")]
    AdminNetworkNotConfigured,

    #[error("all network segments are not allocated yet")]
    NetworkSegmentNotAllocated,

    #[error("network has attached VPC or subdomain : {0}")]
    NetworkSegmentDelete(String),

    #[error(
        "A unique identifier was specified for a new object.  when creating a new object of type {0}, do not specify an identifier"
    )]
    IdentifierSpecifiedForNewObject(String),

    #[error("internal error: {message}")]
    Internal { message: String },

    #[error("only one interface per machine can be marked as primary")]
    OnePrimaryInterface,

    #[error("find one returned no results but should return one for uuid - {0}")]
    FindOneReturnedNoResultsError(uuid::Uuid),

    #[error("find one returned many results but should return one for uuid - {0}")]
    FindOneReturnedManyResultsError(uuid::Uuid),

    #[error("JSON parse failure - {0}")]
    JSONParseError(#[from] serde_json::Error),

    #[error("tokio task join error {0}")]
    TokioJoinError(#[from] tokio::task::JoinError),

    #[error("can not convert between RPC data model and internal data model - {0}")]
    RpcDataConversionError(#[from] RpcDataConversionError),

    #[error("invalid configuration version - {0}")]
    InvalidConfigurationVersion(#[from] ConfigVersionParseError),

    // TODO: Or VersionMismatchError? Or ObjectNotFoundOrModifiedError?
    #[error(
        "an object of type {0} was intended to be modified did not have the expected version {1}"
    )]
    ConcurrentModificationError(&'static str, String),

    #[error("the function is not implemented")]
    NotImplemented,

    #[error("invalid configuration: {0}")]
    InvalidConfiguration(#[from] ConfigValidationError),

    #[error("error in DHCP allocation/handling: {0}")]
    DhcpError(#[from] DhcpError),

    #[error("error in libredfish: {0}")]
    RedfishError(#[from] libredfish::RedfishError),

    #[error("could not create connection to redfish API to {machine_id}, check logs")]
    RedfishClientCreation {
        inner: Box<RedfishClientCreationError>,
        machine_id: MachineId,
    },

    #[error("resource pool error: {0}")]
    ResourcePoolError(#[from] resource_pool::ResourcePoolError),

    #[error("resource pool database error: {0}")]
    ResourcePoolDatabaseError(#[from] ResourcePoolDatabaseError),

    #[error("hardware info error: {0}")]
    HardwareInfoError(#[from] HardwareInfoError),

    #[error("failed to call IBFabricManager: {0}")]
    IBFabricError(String),

    #[error("failed to generate client certificate: {0}")]
    ClientCertificateError(String),

    #[error("DPU reprovisioning is already started: {0}")]
    DpuReprovisioningInProgress(String),

    #[error("tenant handling error: {0}")]
    TenantError(#[from] TenantError),

    #[error("machine is in maintenance mode. cannot allocate instance on it")]
    MaintenanceMode,

    #[error("resource {0} is empty")]
    ResourceExhausted(String),

    #[error("tenant SitePrefix quota reached: {used} of {limit} retained SitePrefixes are in use")]
    TenantSitePrefixQuotaExceeded { used: u32, limit: u32 },

    #[error("host is not available for allocation due to health probe alert")]
    UnhealthyHost,

    #[error("lldp handling error: {0}")]
    LldpError(#[from] LldpError),

    #[error("DPU {0} is missing from host snapshot")]
    MissingDpu(MachineId),

    #[error("attest quote error: {0}")]
    AttestQuoteError(String),

    #[error("attest bind key error: {0}")]
    AttestBindKeyError(String),

    #[error("{requested_ip} resolves to {found_mac} not {requested_mac}")]
    BmcMacIpMismatch {
        /// The BMC endpoint IP requested by the caller
        requested_ip: String,
        /// The BMC MAC address requested by the caller
        requested_mac: String,
        /// The actual BMC MAC address found associated with the endpoint IP
        found_mac: String,
    },

    #[error("{0}")]
    FailedPrecondition(String),

    #[error("failed to map device to dpu: {0}")]
    DpuMappingError(String),

    #[error("client certificate presented has missing information: {0}")]
    ClientCertificateMissingInformation(String),

    #[error("rack manager service error: {0}")]
    RackManagerError(#[from] RackManagerError),

    #[error("maximum one association per interface")]
    MaxOneInterfaceAssociation,

    #[error("DPF error: {0}")]
    DpfError(#[from] carbide_dpf::DpfError),

    #[error("service unavailable: {0}")]
    UnavailableError(String),

    #[error("permission denied: {0}")]
    PermissionDeniedError(String),

    #[error("{0}")]
    AlreadyInProgress(String),

    #[error("attestation error: {0}")]
    AttestationError(String),
}

// Implement From<Infallible> so that we can write generic code that converts between MachineId
// representations using TryFrom, while supporting "non-converting" cases where the caller is
// already passing the right type. (The `impl TryFrom<T> for T` blanket impl uses `Infallible` as
// its error type.)
impl From<Infallible> for CarbideError {
    fn from(_: Infallible) -> Self {
        // We just crash if this is ever called, because the whole point of `Infallible` is that
        // it's an error variant that is never actually constructed. Future rust versions will use
        // `!` as the error type for TryFrom<T> for T, and this whole conversion will become
        // unnecessary (as the compiler will already determine it to be unreachable.)
        unreachable!()
    }
}

impl From<InvalidMachineType> for CarbideError {
    fn from(err: InvalidMachineType) -> Self {
        Self::InvalidArgument(err.to_string())
    }
}

impl From<libnmxc::NmxcError> for CarbideError {
    fn from(e: libnmxc::NmxcError) -> Self {
        match e {
            libnmxc::NmxcError::Status(s) => CarbideError::internal(s.to_string()),
            other => CarbideError::internal(other.to_string()),
        }
    }
}

impl From<ModelError> for CarbideError {
    fn from(e: ModelError) -> Self {
        match e {
            ModelError::DpuMappingError(e) => Self::DpuMappingError(e),
            ModelError::MissingDpu(e) => Self::MissingDpu(e),
            ModelError::DatabaseTypeConversionError(e) => Self::DatabaseTypeConversionError(e),
            ModelError::MissingArgument(e) => Self::MissingArgument(e),
            ModelError::HardwareInfo(e) => Self::HardwareInfoError(e),
            ModelError::InvalidArgument(e) => Self::InvalidArgument(e),
            ModelError::InvalidMachindId(e) => Self::InvalidArgument(e),
        }
    }
}

impl From<DatabaseError> for CarbideError {
    fn from(e: DatabaseError) -> Self {
        use CarbideError::*;
        match e {
            DatabaseError::AddressAlreadyInUse(e) => AddressAlreadyInUse(e),
            DatabaseError::AddressParseError(e) => AddressParseError(e),
            DatabaseError::AdminNetworkNotConfigured => AdminNetworkNotConfigured,
            DatabaseError::AlreadyFoundError { kind, id } => AlreadyFoundError { kind, id },
            DatabaseError::ConcurrentModificationError(type_str, msg) => {
                ConcurrentModificationError(type_str, msg)
            }
            DatabaseError::DhcpError(e) => DhcpError(e),
            DatabaseError::ExpectedHostDuplicateMacAddress(e) => ExpectedHostDuplicateMacAddress(e),
            DatabaseError::ExpectedSwitchDuplicateNvosMacAddress(e) => {
                ExpectedSwitchDuplicateNvosMacAddress(e)
            }
            DatabaseError::FailedPrecondition(e) => FailedPrecondition(e),
            DatabaseError::FindOneReturnedManyResultsError(e) => FindOneReturnedManyResultsError(e),
            DatabaseError::FindOneReturnedNoResultsError(e) => FindOneReturnedNoResultsError(e),
            DatabaseError::GenericErrorFromReport(e) => GenericErrorFromReport(e),
            DatabaseError::HardwareInfoError(e) => HardwareInfoError(e),
            DatabaseError::Internal { message } => Internal { message },
            DatabaseError::InvalidArgument(e) => InvalidArgument(e),
            DatabaseError::InvalidConfiguration(e) => InvalidConfiguration(e),
            DatabaseError::MissingArgument(e) => MissingArgument(e),
            // NVOS intentionally has no target until its first versioned secret
            // is published, so status requests can encounter this normal state.
            DatabaseError::MissingSitewideRotationTarget(CredentialRotationType::Nvos) => {
                FailedPrecondition(
                    "no site-wide NVOS credential rotation target has been published".to_string(),
                )
            }
            // Other credential families are seeded during migration. A missing
            // target for them is a corrupted invariant.
            DatabaseError::MissingSitewideRotationTarget(credential_type) => Internal {
                message: format!(
                    "no site-wide rotation target for credential type: {credential_type:?}"
                ),
            },
            DatabaseError::NetworkParseError(e) => NetworkParseError(e),
            DatabaseError::NetworkSegmentDelete(e) => NetworkSegmentDelete(e),
            DatabaseError::NetworkSegmentDuplicateMacAddress(e) => {
                NetworkSegmentDuplicateMacAddress(e)
            }
            DatabaseError::NetworkSegmentNotAllocated => NetworkSegmentNotAllocated,
            DatabaseError::NotFoundError { kind, id } => NotFoundError { kind, id },
            DatabaseError::NotImplemented => NotImplemented,
            DatabaseError::OnePrimaryInterface => OnePrimaryInterface,
            DatabaseError::ResourceExhausted(e) => ResourceExhausted(e),
            DatabaseError::TenantSitePrefixQuotaExceeded { used, limit } => {
                TenantSitePrefixQuotaExceeded { used, limit }
            }
            DatabaseError::ResourcePoolError(e) => ResourcePoolError(e),
            DatabaseError::RpcUuidConversionError(e) => RpcUuidConversionError(e),
            DatabaseError::Sqlx(e) => DBError(e),
            DatabaseError::TenantError(e) => TenantError(e),
            DatabaseError::UuidConversionError(e) => UuidConversionError(e),
            DatabaseError::MaxOneInterfaceAssociation => MaxOneInterfaceAssociation,
            DatabaseError::TryAgain => Internal {
                message: DatabaseError::TryAgain.to_string(),
            },
        }
    }
}

impl From<EndpointExplorationServiceError> for CarbideError {
    fn from(error: EndpointExplorationServiceError) -> Self {
        match error {
            EndpointExplorationServiceError::Database(error) => error.into(),
            EndpointExplorationServiceError::NotFound { kind, id } => {
                CarbideError::NotFoundError { kind, id }
            }
            EndpointExplorationServiceError::AlreadyInProgress(bmc_ip) => {
                CarbideError::AlreadyInProgress(format!(
                    "endpoint exploration already in progress for {bmc_ip}"
                ))
            }
            suppressed_error @ EndpointExplorationServiceError::Suppressed { .. } => {
                CarbideError::FailedPrecondition(suppressed_error.to_string())
            }
            EndpointExplorationServiceError::ConcurrentModification { kind, version } => {
                CarbideError::ConcurrentModificationError(kind, version)
            }
            background_error @ EndpointExplorationServiceError::BackgroundTaskFailed { .. } => {
                CarbideError::internal(background_error.to_string())
            }
        }
    }
}

impl From<IbError> for CarbideError {
    fn from(e: IbError) -> Self {
        match e {
            IbError::DatabaseError(e) => e.into(),
            IbError::ModelError(e) => e.into(),
            IbError::IBFabricError(msg) => Self::IBFabricError(msg),
            IbError::NotFoundError { kind, id } => Self::NotFoundError { kind, id },
            IbError::InvalidArgument(e) => Self::InvalidArgument(e),
            IbError::CredentialConfigurationError(e) => Self::FailedPrecondition(e),
            IbError::NotImplemented => Self::NotImplemented,
            IbError::Internal { message } => Self::Internal { message },
        }
    }
}

impl CarbideError {
    /// Creates a `Internal` error with the given error message
    pub fn internal(message: String) -> Self {
        CarbideError::Internal { message }
    }

    /// Returns the Postgres SQLSTATE code if this is a database error, or `None` otherwise.
    pub(crate) fn pg_sqlstate(&self) -> Option<String> {
        if let CarbideError::DBError(db::AnnotatedSqlxError {
            source: sqlx::Error::Database(db_err),
            ..
        }) = self
        {
            db_err.code().map(|c| c.into_owned())
        } else {
            None
        }
    }

    /// Returns true if this error is a Postgres deadlock (`40P01`) or
    /// serialization failure (`40001`), both of which are safe to retry.
    pub(crate) fn is_retryable_transaction_error(&self) -> bool {
        matches!(
            self,
            CarbideError::DBError(db::AnnotatedSqlxError {
                source: sqlx::Error::Database(db_err),
                ..
            }) if matches!(db_err.code().as_deref(), Some("40P01") | Some("40001"))
        )
    }
}

impl OperatorError for CarbideError {
    fn operator_error_code(&self) -> ErrorCode {
        use ErrorSubsystem::{Api, Redfish};
        match self {
            CarbideError::InvalidArgument(_)
            | CarbideError::VpcCapability(_)
            | CarbideError::InvalidConfiguration(_)
            | CarbideError::RpcDataConversionError(_)
            | CarbideError::MissingArgument(_)
            | CarbideError::NetworkSegmentDelete(_)
            | CarbideError::BmcMacIpMismatch { .. } => ErrorCode::nico(Api, 400),
            CarbideError::ClientCertificateMissingInformation(_) => ErrorCode::nico(Api, 401),
            CarbideError::PermissionDeniedError(_) => ErrorCode::nico(Api, 403),
            CarbideError::NotFoundError { .. } => ErrorCode::nico(Api, 404),
            CarbideError::AlreadyFoundError { .. } | CarbideError::AlreadyInProgress(_) => {
                ErrorCode::nico(Api, 409)
            }
            CarbideError::MaintenanceMode
            | CarbideError::UnhealthyHost
            | CarbideError::ConcurrentModificationError(_, _)
            | CarbideError::FailedPrecondition(_)
            | CarbideError::ExpectedSwitchDuplicateNvosMacAddress(_)
            | CarbideError::AddressAlreadyInUse(_) => ErrorCode::nico(Api, 412),
            CarbideError::ResourceExhausted(_)
            | CarbideError::TenantSitePrefixQuotaExceeded { .. }
            | CarbideError::DhcpError(_) => ErrorCode::nico(Api, 429),
            CarbideError::UnavailableError(_) => ErrorCode::nico(Api, 503),
            CarbideError::RedfishError(error) if is_dpu_bios_attributes_not_ready(error) => {
                EndpointExplorationError::INVALID_DPU_REDFISH_BIOS_RESPONSE_CODE
            }
            CarbideError::RedfishError(_) | CarbideError::RedfishClientCreation { .. } => {
                ErrorCode::nico(Redfish, 500)
            }
            _ => ErrorCode::nico(Api, 500),
        }
    }

    fn operator_mitigation(&self) -> Option<&'static str> {
        match self {
            CarbideError::RedfishError(error) if is_dpu_bios_attributes_not_ready(error) => {
                Some(EndpointExplorationError::INVALID_DPU_REDFISH_BIOS_RESPONSE_MITIGATION)
            }
            CarbideError::RedfishError(_) | CarbideError::RedfishClientCreation { .. } => {
                Some("Check BMC reachability, credentials, and Redfish service health.")
            }
            CarbideError::UnavailableError(_) => Some(
                "A dependent NICo service is temporarily unavailable. Retry the same request \
                 once it recovers: re-run the Admin CLI command, REST API call, or client \
                 integration that failed, backing off briefly between attempts.",
            ),
            CarbideError::ResourceExhausted(_) | CarbideError::DhcpError(_) => {
                Some("Check configured resource pools and available capacity.")
            }
            CarbideError::TenantSitePrefixQuotaExceeded { .. } => Some(
                "Review the tenant's retained SitePrefixes; complete removal of an unneeded prefix \
                 or increase max_site_prefixes_per_tenant if additional roots are intended.",
            ),
            _ => None,
        }
    }
}

#[test]
fn test_carbide_error() {
    let error = crate::CarbideError::internal(String::from("unable to yeet foo into the sun"));
    assert_eq!(
        error.to_string(),
        "internal error: unable to yeet foo into the sun"
    );
}

impl From<::measured_boot::Error> for CarbideError {
    fn from(value: measured_boot::Error) -> Self {
        CarbideError::internal(value.to_string())
    }
}

impl From<CarbideError> for tonic::Status {
    #[track_caller] // get the source Location from the caller, not this function
    fn from(error: CarbideError) -> Self {
        let schema = error.operator_error_schema();

        // TODO: There's many more mapped to `Status::internal` which are likely
        // user errors instead
        let mut status = match &error {
            e @ CarbideError::Internal { .. } => Status::internal(e.to_string()),
            CarbideError::InvalidArgument(msg) => Status::invalid_argument(msg),
            error @ CarbideError::VpcCapability(_) => Status::invalid_argument(error.to_string()),
            CarbideError::InvalidConfiguration(e) => Status::invalid_argument(e.to_string()),
            CarbideError::RpcDataConversionError(e) => Status::invalid_argument(e.to_string()),
            e @ CarbideError::DhcpError(_) => Status::resource_exhausted(e.to_string()),
            CarbideError::MissingArgument(msg) => Status::invalid_argument(*msg),
            CarbideError::NetworkSegmentDelete(msg) => Status::invalid_argument(msg),
            CarbideError::NotFoundError { kind, id } => {
                Status::not_found(format!("{kind} not found: {id}"))
            }
            CarbideError::AlreadyFoundError { kind, id } => {
                Status::already_exists(format!("{kind} already exists: {id}"))
            }
            CarbideError::MaintenanceMode => {
                Status::failed_precondition("MaintenanceMode".to_string())
            }
            e @ CarbideError::BmcMacIpMismatch { .. } => Status::invalid_argument(e.to_string()),
            CarbideError::UnhealthyHost => Status::failed_precondition(error.to_string()),
            CarbideError::ResourceExhausted(kind) => Status::resource_exhausted(kind),
            error @ CarbideError::TenantSitePrefixQuotaExceeded { .. } => {
                Status::resource_exhausted(error.to_string())
            }
            error @ CarbideError::ConcurrentModificationError(_, _) => {
                Status::failed_precondition(error.to_string())
            }
            error @ CarbideError::FailedPrecondition(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ CarbideError::ExpectedSwitchDuplicateNvosMacAddress(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ CarbideError::AddressAlreadyInUse(_) => {
                Status::failed_precondition(error.to_string())
            }
            error @ CarbideError::ClientCertificateMissingInformation(_) => {
                Status::unauthenticated(error.to_string())
            }
            CarbideError::UnavailableError(msg) => Status::unavailable(msg),
            CarbideError::PermissionDeniedError(msg) => Status::permission_denied(msg),
            CarbideError::AlreadyInProgress(msg) => Status::already_exists(msg),
            other => Status::internal(other.to_string()),
        };

        insert_ascii_metadata(
            &mut status,
            "nico-error-code",
            &schema.error_code.to_string(),
        );
        insert_ascii_metadata(&mut status, "nico-error-text", &schema.text);
        if let Some(mitigation) = &schema.mitigation {
            insert_ascii_metadata(&mut status, "nico-error-mitigation", mitigation);
        }

        let error_with_location = CarbideErrorWithLocation {
            error,
            location: Location::caller().to_string(),
        };

        // Set the inner error to the CarbideError, which can be inspected by our LogService layer
        status.set_source(Arc::new(error_with_location));
        status
    }
}

/// A CarbideError with the corresponding source location where it was converted to a tonic::Status
#[derive(Debug)]
pub(crate) struct CarbideErrorWithLocation {
    pub(crate) error: CarbideError,
    pub(crate) location: String,
}

impl Display for CarbideErrorWithLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.error, f)
    }
}

impl std::error::Error for CarbideErrorWithLocation {}

fn insert_ascii_metadata(status: &mut Status, key: &'static str, value: &str) {
    match MetadataValue::try_from(value) {
        Ok(value) => {
            status.metadata_mut().insert(key, value);
        }
        Err(error) => {
            tracing::warn!(
                metadata_key = key,
                error = %error,
                "Operator error metadata was rejected because it contains non-ASCII content"
            );
        }
    }
}

/// Result type for the return type of Carbide functions
///
/// Wraps `CarbideError` into `CarbideResult<T>`
pub(crate) type CarbideResult<T> = Result<T, CarbideError>;

#[test]
fn test_carbide_result() {
    use crate::{CarbideError, CarbideResult};

    fn do_something() -> CarbideResult<u8> {
        Err(CarbideError::internal(String::from("can't make u8")))
    }
    assert!(matches!(do_something(), Err(CarbideError::Internal { .. })));
}

#[test]
fn test_dhcp_error_maps_to_resource_exhausted_status() {
    let err = CarbideError::DhcpError(DhcpError::PrefixExhausted(
        "10.217.5.160".parse().expect("valid IP"),
    ));
    let status: tonic::Status = err.into();
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
}

#[test]
fn tenant_site_prefix_quota_error_is_actionable() {
    let err = CarbideError::TenantSitePrefixQuotaExceeded { used: 8, limit: 8 };
    let status: tonic::Status = err.into();

    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert_eq!(
        status.message(),
        "tenant SitePrefix quota reached: 8 of 8 retained SitePrefixes are in use"
    );
    assert_eq!(
        status
            .metadata()
            .get("nico-error-code")
            .expect("metadata should include operator error code")
            .to_str()
            .expect("operator error code should be ASCII"),
        "NICO-API-429"
    );
    assert_eq!(
        status
            .metadata()
            .get("nico-error-text")
            .expect("metadata should include operator error text")
            .to_str()
            .expect("operator error text should be ASCII"),
        "tenant SitePrefix quota reached: 8 of 8 retained SitePrefixes are in use"
    );
    assert_eq!(
        status
            .metadata()
            .get("nico-error-mitigation")
            .expect("metadata should include operator mitigation")
            .to_str()
            .expect("operator mitigation should be ASCII"),
        "Review the tenant's retained SitePrefixes; complete removal of an unneeded prefix or \
         increase max_site_prefixes_per_tenant if additional roots are intended."
    );
}

#[test]
fn test_unavailable_error_maps_to_unavailable_status() {
    let err = CarbideError::UnavailableError("service down".into());
    let status: tonic::Status = err.into();
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[test]
fn unavailable_error_schema_describes_who_should_retry() {
    let schema = CarbideError::UnavailableError("service down".into()).operator_error_schema();

    let mitigation = schema.mitigation.expect("has a mitigation");
    assert!(mitigation.contains("Admin CLI"));
    assert!(mitigation.contains("REST API"));
}

#[test]
fn invalid_argument_status_includes_operator_schema_metadata() {
    let err = CarbideError::InvalidArgument("bad input".into());
    let status: tonic::Status = err.into();

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        status
            .metadata()
            .get("nico-error-code")
            .expect("metadata should include operator error code")
            .to_str()
            .expect("operator error code should be ASCII"),
        "NICO-API-400"
    );
    assert_eq!(
        status
            .metadata()
            .get("nico-error-text")
            .expect("metadata should include operator error text")
            .to_str()
            .expect("operator error text should be ASCII"),
        "argument is invalid: bad input"
    );
}

#[test]
fn dpu_bios_redfish_error_uses_dpu_operator_schema() {
    let err = CarbideError::RedfishError(libredfish::RedfishError::MissingKey {
        key: "HostPrivilegeLevel".to_string(),
        url: "Systems/{}/Bios".to_string(),
    });

    let schema = err.operator_error_schema();

    assert_eq!(
        schema.error_code,
        EndpointExplorationError::INVALID_DPU_REDFISH_BIOS_RESPONSE_CODE
    );
    assert_eq!(
        schema.mitigation.as_deref(),
        Some(EndpointExplorationError::INVALID_DPU_REDFISH_BIOS_RESPONSE_MITIGATION)
    );
}

#[test]
fn test_permission_denied_error_maps_to_permission_denied_status() {
    let err = CarbideError::PermissionDeniedError("not allowed".into());
    let status: tonic::Status = err.into();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[test]
fn test_address_already_in_use_maps_to_failed_precondition_status() {
    use std::str::FromStr;
    let err = CarbideError::AddressAlreadyInUse(AddressAlreadyInUseError(
        "10.0.0.1".parse().unwrap(),
        MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap(),
        uuid::Uuid::new_v4().into(),
        uuid::Uuid::new_v4().into(),
    ));
    let status: tonic::Status = err.into();
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
}

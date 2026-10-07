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

use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineIdParseError};
use carbide_uuid::site_prefix::SitePrefixId;
use carbide_uuid::switch::{SwitchId, SwitchIdParseError};
use rpc::forge_tls_client::ForgeTlsClientError;

#[derive(thiserror::Error, Debug)]
pub(crate) enum CarbideCliError {
    #[error("unable to connect to carbide API: {0}")]
    ApiConnectFailed(#[from] ForgeTlsClientError),

    #[error("the API call to the forge API server returned {0}")]
    ApiInvocationError(#[from] tonic::Status),

    #[error("ufm credential command is unavailable: {0}")]
    UfmCredentialCommandUnavailable(String),

    #[error("error while writing into string: {0}")]
    StringWriteError(#[from] std::fmt::Error),

    #[error("generic error: {0}")]
    GenericError(String),

    #[error("operation not allowed due to potential inconsistencies with cloud database")]
    CloudUnsafeOp,

    #[error("cannot specify both {0} and {1}. please provide only one")]
    ChooseOneError(&'static str, &'static str),

    #[error("must specify either {0} or {1}")]
    RequireOneError(&'static str, &'static str),

    #[error("invalid datetime format: {0}. use 'YYYY-MM-DD HH:MM:SS' or 'HH:MM:SS'")]
    InvalidDateTimeFromUserInput(String),

    #[error("segment not found")]
    SegmentNotFound,

    #[error("domain not found")]
    DomainNotFound,

    #[error("uuid not found")]
    UuidNotFound,

    #[error("MAC not found")]
    MacAddressNotFound,

    #[error("serial number not found")]
    SerialNumberNotFound,

    #[error("error while handling json: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("error while handling yaml: {0}")]
    YamlError(#[from] serde_yaml::Error),

    #[error("error while handling csv: {0}")]
    CsvError(#[from] csv::Error),

    #[error("machine with id {0} not found")]
    MachineNotFound(MachineId),

    #[error("switch with id {0} not found")]
    SwitchNotFound(SwitchId),

    #[error("remediation with id {0} not found")]
    RemediationNotFound(RemediationId),

    #[error("instance with id {0} not found")]
    InstanceNotFound(InstanceId),

    #[error("tenant with id {0} not found")]
    TenantNotFound(String),

    #[error("no SitePrefix found with id {0}")]
    SitePrefixNotFound(SitePrefixId),

    #[error("I/O error. does the file exist? {0}")]
    IOError(#[from] std::io::Error),

    /// For when you expected some values but the response was empty.
    /// If empty is acceptable don't use this.
    #[error("no results returned")]
    Empty,

    #[error("not implemented {0}")]
    NotImplemented(String),

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(&'static str),

    #[error("invalid machine id: {0}")]
    InvalidMachineId(#[from] MachineIdParseError),

    #[error("invalid switch id: {0}")]
    InvalidSwitchId(#[from] SwitchIdParseError),

    #[error("RPC data conversion error: {0}")]
    RpcDataConversionError(#[from] ::rpc::errors::RpcDataConversionError),

    #[error(transparent)]
    EyreReport(eyre::Report),
}

impl From<eyre::Report> for CarbideCliError {
    // For commands that are [still] returning an eyre::Report,
    // and not a CarbideCliError, preserve the full report and
    // error chain for complete context.
    fn from(err: eyre::Report) -> Self {
        CarbideCliError::EyreReport(err)
    }
}

pub(crate) type CarbideCliResult<T> = Result<T, CarbideCliError>;

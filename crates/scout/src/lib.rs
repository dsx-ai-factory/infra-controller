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

use std::num::ParseIntError;

use carbide_utils::cmd::CmdError;
use carbide_uuid::machine::InvalidMachineType;

#[derive(thiserror::Error, Debug)]
pub enum CarbideClientError {
    #[error("generic error: {0}")]
    GenericError(String),

    #[error("generic transport error {0}")]
    TransportError(String),

    #[error("generic tonic status error {0}")]
    TonicStatusError(#[from] tonic::Status),

    #[error("regex error {0}")]
    RegexError(#[from] regex::Error),

    #[error("StdIo error {0}")]
    StdIo(#[from] std::io::Error),

    #[error("hardware enumeration error: {0}")]
    HardwareEnumerationError(
        #[from] carbide_host_support::hardware_enumeration::HardwareEnumerationError,
    ),

    #[error("registration error: {0}")]
    RegistrationError(#[from] carbide_host_support::registration::RegistrationError),

    #[error("subprocess failed: {0}")]
    SubprocessError(#[from] CmdError),

    #[error("NVME parsing failed: {0}")]
    NvmeParsingError(#[from] ParseIntError),

    #[error("TPM error: {0}")]
    TpmError(String),
}

pub type CarbideClientResult<T> = Result<T, CarbideClientError>;

impl From<InvalidMachineType> for CarbideClientError {
    fn from(value: InvalidMachineType) -> Self {
        Self::GenericError(format!("invalid machine type: {value}"))
    }
}

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

use clap::{Parser, ValueEnum};

use crate::errors::CarbideCliError;

pub(super) const DEFAULT_IB_FABRIC_NAME: &str = "default";

/// Maps a failed-precondition response to the operator-facing UFM command
/// error, while preserving the standard API error mapping for other statuses.
pub(super) fn map_ufm_credential_api_error(status: tonic::Status) -> CarbideCliError {
    if status.code() == tonic::Code::FailedPrecondition {
        CarbideCliError::UfmCredentialCommandUnavailable(status.message().to_string())
    } else {
        status.into()
    }
}

#[derive(ValueEnum, Parser, Debug, Clone)]
pub(super) enum BmcCredentialType {
    // Site Wide BMC Root Account Credentials
    SiteWideRoot,
    // BMC Specific Root Credentials
    BmcRoot,
    // BMC Specific Forge-Admin Credentials
    BmcForgeAdmin,
}

impl From<BmcCredentialType> for rpc::forge::CredentialType {
    fn from(c_type: BmcCredentialType) -> Self {
        use rpc::forge::CredentialType::*;
        match c_type {
            BmcCredentialType::SiteWideRoot => SiteWideBmcRoot,
            BmcCredentialType::BmcRoot => RootBmcByMacAddress,
            BmcCredentialType::BmcForgeAdmin => BmcForgeAdminByMacAddress,
        }
    }
}

#[derive(ValueEnum, Parser, Debug, Clone)]
pub(super) enum UefiCredentialType {
    Dpu,
    Host,
}

/// Credential families an operator can target for site-wide rotation. These map
/// 1:1 onto `rpc::forge::RotationCredentialType` (minus its proto3 `Unspecified`
/// zero value).
///
/// NVOS is listed because the server can stage site-wide NVOS targets. Device
/// convergence depends on the switch-controller and component-manager path.
///
/// DpuUefi is the DPU *UEFI* password; DpuBmcService is the BF4 DPU *BMC*
/// `service` account -- distinct credentials on the same device. Only BF4 DPUs
/// expose the `service` account, so a `dpu-bmc-service` rotation converges just
/// the BF4 DPU BMCs.
#[derive(ValueEnum, Parser, Debug, Clone)]
pub(super) enum RotationCredentialKind {
    Bmc,
    HostUefi,
    DpuUefi,
    Nvos,
    LockdownIkm,
    DpuBmcService,
}

impl From<RotationCredentialKind> for rpc::forge::RotationCredentialType {
    fn from(kind: RotationCredentialKind) -> Self {
        use rpc::forge::RotationCredentialType::*;
        match kind {
            RotationCredentialKind::Bmc => RotationBmc,
            RotationCredentialKind::HostUefi => RotationHostUefi,
            RotationCredentialKind::DpuUefi => RotationDpuUefi,
            RotationCredentialKind::Nvos => RotationNvos,
            RotationCredentialKind::LockdownIkm => RotationLockdownIkm,
            RotationCredentialKind::DpuBmcService => RotationDpuBmcService,
        }
    }
}

impl From<UefiCredentialType> for rpc::forge::CredentialType {
    fn from(c_type: UefiCredentialType) -> Self {
        use rpc::forge::CredentialType::*;
        match c_type {
            UefiCredentialType::Dpu => DpuUefi,
            UefiCredentialType::Host => HostUefi,
        }
    }
}

pub(super) fn url_validator(url: String) -> Result<String, CarbideCliError> {
    let addr = tonic::transport::Uri::try_from(&url)
        .map_err(|_| CarbideCliError::GenericError("invalid url".to_string()))?;
    Ok(addr.to_string())
}

pub(super) fn password_validator(s: String) -> Result<String, CarbideCliError> {
    // TODO: check password according BMC pwd rule.
    if s.is_empty() {
        return Err(CarbideCliError::GenericError("invalid input".to_string()));
    }
    Ok(s)
}

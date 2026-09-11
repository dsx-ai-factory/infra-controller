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

mod add_bmc;
mod add_dpu_factory_default;
mod add_host_factory_default;
mod add_nic_lockdown_ikm;
mod add_nmxm;
mod add_uefi;
mod add_ufm;
mod bgp;
mod common;
mod delete_bmc;
mod delete_nmxm;
mod delete_ufm;
mod firmware_access_token;
mod force_bmc;
mod force_uefi;
mod generate_ufm_cert;
mod registry;
mod rotate;
mod rotation_status;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

pub(crate) const NMX_M_UNSUPPORTED_MESSAGE: &str =
    "NMX-M is no longer supported; use NMX-C for NVLink partition management";

#[derive(Parser, Debug, Clone, Dispatch)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Cmd {
    #[clap(about = "Add UFM credential")]
    AddUFM(add_ufm::Args),
    #[clap(about = "Delete UFM credential")]
    DeleteUFM(delete_ufm::Args),
    #[clap(about = "Generate UFM credential")]
    GenerateUFMCert(generate_ufm_cert::Args),
    #[clap(about = "Add BMC credentials")]
    AddBMC(add_bmc::Args),
    #[clap(about = "Delete BMC credentials")]
    DeleteBMC(delete_bmc::Args),
    #[clap(about = "Set the site-wide SuperNIC lockdown IKM (input key material)")]
    AddNicLockdownIkm(add_nic_lockdown_ikm::Args),
    #[clap(
        about = "Add site-wide DPU UEFI default credential (NOTE: this parameter can be set only once)"
    )]
    AddUefi(add_uefi::Args),
    #[clap(about = "Add manufacturer factory default BMC user/pass for a given vendor")]
    AddHostFactoryDefault(add_host_factory_default::Args),
    #[clap(about = "Add manufacturer factory default BMC user/pass for the DPUs")]
    AddDpuFactoryDefault(add_dpu_factory_default::Args),
    #[clap(about = "Deprecated compatibility command; NMX-M is no longer supported")]
    AddNmxM(add_nmxm::Args),
    #[clap(about = "Deprecated compatibility command; NMX-M is no longer supported")]
    DeleteNmxM(delete_nmxm::Args),
    #[clap(about = "Manage leaf BGP passwords", subcommand)]
    Bgp(bgp::Cmd),
    #[clap(about = "Manage container registry credentials", subcommand)]
    Registry(registry::Args),
    #[clap(about = "Manage firmware artifact access tokens", subcommand)]
    FirmwareAccessToken(firmware_access_token::Args),
    #[clap(about = "Stage a site-wide credential rotation (auto-generate or explicit password)")]
    Rotate(rotate::Args),
    #[clap(about = "Show convergence status of a site-wide credential rotation")]
    RotationStatus(rotation_status::Args),
    #[clap(
        about = "Force-converge a single BMC's credentials now (operator escape hatch)",
        subcommand
    )]
    ForceBmc(force_bmc::Args),
    #[clap(
        about = "Force-converge a single machine's UEFI credential now (operator escape hatch)",
        subcommand
    )]
    ForceUefi(force_uefi::Args),
}

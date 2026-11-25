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

mod allocate;
mod reboot;
mod release;
mod show;
mod update_extension_services;
mod update_ib_config;
mod update_nvlink_config;
mod update_os;
mod update_spx_config;

// Cross-module re-exports for jump module
// Cross-module re-export for rpc module
pub(crate) use allocate::args::Args as AllocateInstance;
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use model::instance::config::extension_services::RequestedInstanceExtensionServiceConfig;
use rpc::forge::InstanceDpuExtensionServiceConfig;
pub(crate) use show::args::Args as ShowInstance;
pub(crate) use show::cmd::handle_show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Display instance information")]
    Show(show::Args),
    #[clap(about = "Reboot instance, potentially applying firmware updates")]
    Reboot(reboot::Args),
    #[clap(about = "De-allocate instance")]
    Release(release::Args),
    #[clap(about = "Allocate instance")]
    Allocate(allocate::Args),
    #[clap(about = "Update instance OS")]
    UpdateOS(update_os::Args),
    #[clap(about = "Replace an instance's extension services")]
    UpdateExtensionServices(update_extension_services::Args),
    #[clap(about = "Update instance IB configuration")]
    UpdateIbConfig(update_ib_config::Args),
    #[clap(about = "Update instance NVLink configuration")]
    UpdateNvLinkConfig(update_nvlink_config::Args),
    #[clap(about = "Update instance SPX configuration")]
    UpdateSpxConfig(update_spx_config::Args),
}

impl Cmd {
    /// Validates attachment arguments before allocation or replacement can contact Core.
    pub(crate) fn validate_extension_service_arguments(
        &self,
        cloud_unsafe_op_present: bool,
    ) -> Result<(), clap::Error> {
        let (services, command, path): (&[_], fn() -> clap::Command, _) = match self {
            Self::UpdateExtensionServices(_) if !cloud_unsafe_op_present => {
                return Err(update_extension_services::Args::command()
                    .bin_name("nico-admin-cli instance update-extension-services")
                    .error(
                        ErrorKind::MissingRequiredArgument,
                        "supply --cloud-unsafe-op <USERNAME> before instance",
                    ));
            }
            Self::Allocate(args) => (
                &args.extension_services,
                allocate::Args::command,
                "nico-admin-cli instance allocate",
            ),
            Self::UpdateExtensionServices(args) => (
                &args.extension_services,
                update_extension_services::Args::command,
                "nico-admin-cli instance update-extension-services",
            ),
            _ => return Ok(()),
        };
        if carbide_utils::has_duplicates(services.iter().map(|service| &service.service_id)) {
            return Err(command().bin_name(path).error(
                ErrorKind::ArgumentConflict,
                "each --extension-service must specify a different service ID",
            ));
        }
        Ok(())
    }
}

/// Deserializes and validates one attachment request so malformed selections fail during parsing.
fn parse_extension_service(value: &str) -> Result<InstanceDpuExtensionServiceConfig, String> {
    let selection: InstanceDpuExtensionServiceConfig =
        serde_json::from_str(value).map_err(|error| error.to_string())?;
    let validated = RequestedInstanceExtensionServiceConfig::try_from(selection.clone())
        .map_err(|error| error.to_string())?;

    // Use the canonical service ID for duplicate detection across flags.
    Ok(InstanceDpuExtensionServiceConfig {
        service_id: validated.service_id.to_string(),
        version: validated.version.to_string(),
        ..selection
    })
}

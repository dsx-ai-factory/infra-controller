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

use ::rpc::forge::host_reprovisioning_request::Mode;
use ::rpc::forge::{HostReprovisioningRequest, UpdateInitiator};
use carbide_uuid::machine::StableHostMachineId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a host into reprovisioning mode:
    $ nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567

Clear reprovisioning mode for a host:
    $ nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567

List all hosts pending reprovisioning:
    $ nico-admin-cli host reprovision list

")]
pub(crate) enum Args {
    #[clap(about = "Set the host in reprovisioning mode.")]
    Set(ReprovisionSet),
    #[clap(about = "Clear the reprovisioning mode.")]
    Clear(ReprovisionClear),
    #[clap(about = "List all hosts pending reprovisioning.")]
    List,
    // TODO: Remove when manual upgrade feature is removed
    #[clap(about = "Mark manual firmware upgrade as complete for a host.")]
    MarkManualUpgradeComplete(ManualFirmwareUpgradeComplete),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a host into reprovisioning mode:
    $ nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567

Set into reprovisioning mode and update firmware:
    $ nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567 \
    --update-firmware

Set into reprovisioning mode and raise a health alert with a message:
    $ nico-admin-cli host reprovision set --id 12345678-1234-5678-90ab-cdef01234567 \
    --update-message \"Quarterly firmware refresh\"

")]
pub(crate) struct ReprovisionSet {
    #[clap(short, long, help = "Machine ID for which reprovisioning is needed.")]
    pub(super) id: StableHostMachineId,

    #[clap(short, long, action)]
    update_firmware: bool,

    #[clap(
        long,
        alias = "maintenance_reference",
        help = "If set, a HostUpdateInProgress health alert will be applied to the host"
    )]
    pub(super) update_message: Option<String>,
}

impl From<&ReprovisionSet> for HostReprovisioningRequest {
    fn from(args: &ReprovisionSet) -> Self {
        Self {
            machine_id: Some(args.id),
            mode: Mode::Set as i32,
            initiator: UpdateInitiator::AdminCli as i32,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear reprovisioning mode for a host:
    $ nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567

Clear reprovisioning mode and update firmware:
    $ nico-admin-cli host reprovision clear --id 12345678-1234-5678-90ab-cdef01234567 \
    --update-firmware

")]
pub(crate) struct ReprovisionClear {
    #[clap(
        short,
        long,
        help = "Machine ID for which reprovisioning should be cleared."
    )]
    id: StableHostMachineId,

    #[clap(short, long, action)]
    update_firmware: bool,
}

impl From<ReprovisionClear> for HostReprovisioningRequest {
    fn from(args: ReprovisionClear) -> Self {
        Self {
            machine_id: Some(args.id),
            mode: Mode::Clear as i32,
            initiator: UpdateInitiator::AdminCli as i32,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Mark manual firmware upgrade complete for a host:
    $ nico-admin-cli host reprovision mark-manual-upgrade-complete \
    --id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ManualFirmwareUpgradeComplete {
    #[clap(
        short,
        long,
        help = "Machine ID for which manual firmware upgrade should be set."
    )]
    pub(super) id: StableHostMachineId,
}

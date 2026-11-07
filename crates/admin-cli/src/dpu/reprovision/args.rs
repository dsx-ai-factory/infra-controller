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

use carbide_uuid::machine::MachineId;
use clap::Parser;
use rpc::forge::dpu_reprovisioning_request::Mode;
use rpc::forge::{DpuReprovisioningRequest, UpdateInitiator};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all DPUs pending reprovisioning:
    $ nico-admin-cli dpu reprovision list

Set a DPU into reprovisioning mode:
    $ nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567

Clear reprovisioning mode for a DPU:
    $ nico-admin-cli dpu reprovision clear --id 12345678-1234-5678-90ab-cdef01234567

Restart reprovisioning for a host:
    $ nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum Args {
    #[clap(about = "Set the DPU in reprovisioning mode.")]
    Set(DpuReprovisionSet),
    #[clap(about = "Clear the reprovisioning mode.")]
    Clear(DpuReprovisionClear),
    #[clap(about = "List all DPUs pending reprovisioning.")]
    List,
    #[clap(about = "Restart the DPU reprovision.")]
    Restart(DpuReprovisionRestart),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Set a single DPU into reprovisioning mode:
    $ nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567

Reprovision all DPUs on a host by passing the host machine id:
    $ nico-admin-cli dpu reprovision set --id abcdef01-2345-6789-abcd-ef0123456789

Reprovision and update DPU firmware, recording a maintenance message:
    $ nico-admin-cli dpu reprovision set --id 12345678-1234-5678-90ab-cdef01234567 \
    --update-firmware --update-message \"scheduled firmware refresh\"

")]
pub(crate) struct DpuReprovisionSet {
    #[clap(
        short,
        long,
        help = "DPU Machine ID for which reprovisioning is needed, or host machine id if all DPUs should be reprovisioned."
    )]
    pub(super) id: MachineId,

    #[clap(short, long, action)]
    update_firmware: bool,

    #[clap(
        long,
        alias = "maintenance_reference",
        help = "If set, a HostUpdateInProgress health alert will be applied to the host"
    )]
    pub(super) update_message: Option<String>,
}

impl From<&DpuReprovisionSet> for DpuReprovisioningRequest {
    fn from(args: &DpuReprovisionSet) -> Self {
        Self {
            dpu_id: Some(args.id),
            machine_id: Some(args.id),
            mode: Mode::Set as i32,
            initiator: UpdateInitiator::AdminCli as i32,
            update_firmware: args.update_firmware,
        }
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Clear reprovisioning mode for a single DPU:
    $ nico-admin-cli dpu reprovision clear --id 12345678-1234-5678-90ab-cdef01234567

Clear reprovisioning for all DPUs on a host by passing the host machine id:
    $ nico-admin-cli dpu reprovision clear --id abcdef01-2345-6789-abcd-ef0123456789

")]
pub(crate) struct DpuReprovisionClear {
    #[clap(
        short,
        long,
        help = "DPU Machine ID for which reprovisioning should be cleared, or host machine id if all DPUs should be cleared."
    )]
    id: MachineId,

    #[clap(short, long, action)]
    update_firmware: bool,
}

impl From<&DpuReprovisionClear> for DpuReprovisioningRequest {
    fn from(args: &DpuReprovisionClear) -> Self {
        Self {
            dpu_id: Some(args.id),
            machine_id: Some(args.id),
            mode: Mode::Clear as i32,
            initiator: UpdateInitiator::AdminCli as i32,
            update_firmware: args.update_firmware,
        }
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Restart reprovisioning for a host:
    $ nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567

Restart reprovisioning and update DPU firmware:
    $ nico-admin-cli dpu reprovision restart --id 12345678-1234-5678-90ab-cdef01234567 \
    --update-firmware

")]
pub(crate) struct DpuReprovisionRestart {
    #[clap(
        short,
        long,
        help = "Host Machine ID for which reprovisioning should be restarted."
    )]
    id: MachineId,

    #[clap(short, long, action)]
    update_firmware: bool,
}

impl From<&DpuReprovisionRestart> for DpuReprovisioningRequest {
    fn from(args: &DpuReprovisionRestart) -> Self {
        Self {
            dpu_id: Some(args.id),
            machine_id: Some(args.id),
            mode: Mode::Restart as i32,
            initiator: UpdateInitiator::AdminCli as i32,
            update_firmware: args.update_firmware,
        }
    }
}

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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show a machine's hardware info:
    $ nico-admin-cli machine hardware-info show --machine 12345678-1234-5678-90ab-cdef01234567

Update a machine's GPUs from a JSON file:
    $ nico-admin-cli machine hardware-info update gpus --machine 12345678-1234-5678-90ab-cdef01234567 \
    --gpu-json-file ./gpus.json

")]
pub(crate) enum Args {
    #[clap(about = "Show the hardware info of the machine")]
    Show(ShowMachineHardwareInfo),
    #[clap(subcommand, about = "Update the hardware info of the machine")]
    Update(MachineHardwareInfo),
}

#[derive(Parser, Debug)]
pub(crate) struct ShowMachineHardwareInfo {
    #[clap(long, help = "Show the hardware info of this Machine ID")]
    pub(super) machine: MachineId,
}

#[derive(Parser, Debug)]
pub(crate) enum MachineHardwareInfo {
    //Cpu(MachineTopologyCommandCpu),
    #[clap(about = "Update the GPUs of this machine")]
    Gpus(MachineHardwareInfoGpus),
    //Memory(MachineTopologyCommandMemory),
    //Storage(MachineTopologyCommandStorage),
    //Network(MachineTopologyCommandNetwork),
    //Infiniband(MachineTopologyCommandInfiniband),
    //Dpu(MachineTopologyCommandDpu),
}

#[derive(Parser, Debug)]
pub(crate) struct MachineHardwareInfoGpus {
    #[clap(long, help = "Machine ID of the server containing the GPUs")]
    pub(super) machine: MachineId,
    #[clap(
        long,
        help = "JSON file containing GPU info. It should contain an array of JSON objects like this:
        {
            \"name\": \"string\",
            \"serial\": \"string\",
            \"driver_version\": \"string\",
            \"vbios_version\": \"string\",
            \"inforom_version\": \"string\",
            \"total_memory\": \"string\",
            \"frequency\": \"string\",
            \"pci_bus_id\": \"string\"
        }
        Pass an empty array if you want to remove GPUs."
    )]
    pub(super) gpu_json_file: std::path::PathBuf,
}

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

use carbide_uuid::machine::HostMachineId;
use clap::Parser;
use rpc::forge::DpaInterfaceType;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create/ensure a DPA interface for a machine:
    $ nico-admin-cli dpa ensure 12345678-1234-5678-90ab-cdef01234567 \
    00:11:22:33:44:55 BlueField3 5e:00.0

Create/ensure a DPA interface with a device description:
    $ nico-admin-cli dpa ensure 12345678-1234-5678-90ab-cdef01234567 \
    00:11:22:33:44:55 BlueField3 5e:00.0 \"NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC\"

")]
pub(crate) struct Args {
    #[clap(help = "Machine ID")]
    machine_id: HostMachineId,
    #[clap(help = "MAC address (e.g. 00:11:22:33:44:55)")]
    mac_addr: String,
    #[clap(help = "Device type (e.g. BlueField3)")]
    device_type: String,
    #[clap(help = "PCI name (e.g. 5e:00.0)")]
    pci_name: String,
    #[clap(help = "Interface type (e.g. SVPC or ASTRA)", value_enum)]
    interface_type: DpaInterfaceType,
    #[clap(help = "Device description (e.g. NVIDIA BlueField-3 B3140L E-Series FHHL SuperNIC)")]
    device_description: Option<String>,
}

impl From<Args> for ::rpc::forge::DpaInterfaceCreationRequest {
    fn from(args: Args) -> Self {
        Self {
            machine_id: Some(args.machine_id),
            mac_addr: args.mac_addr,
            device_type: args.device_type,
            pci_name: args.pci_name,
            device_description: args.device_description,
            interface_type: args.interface_type.into(),
        }
    }
}

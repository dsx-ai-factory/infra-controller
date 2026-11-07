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

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set the Name or Description of the Machine")]
    Set(MachineMetadataCommandSet),
    #[clap(about = "Show the Metadata of the Machine")]
    Show(MachineMetadataCommandShow),
    #[clap(about = "Adds a label to the Metadata of a Machine")]
    AddLabel(MachineMetadataCommandAddLabel),
    #[clap(about = "Removes labels from the Metadata of a Machine")]
    RemoveLabels(MachineMetadataCommandRemoveLabels),
    #[clap(about = "Copy Machine Metadata from Expected-Machine to Machine")]
    FromExpectedMachine(MachineMetadataCommandFromExpectedMachine),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show a machine's metadata:
    $ nico-admin-cli machine metadata show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct MachineMetadataCommandShow {
    #[clap(help = "The machine which should get updated metadata")]
    pub(super) machine: MachineId,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a machine's name and description:
    $ nico-admin-cli machine metadata set 12345678-1234-5678-90ab-cdef01234567 \
    --name gpu-node-01 --description \"Rack 4, tray 2\"

")]
pub(crate) struct MachineMetadataCommandSet {
    #[clap(help = "The machine which should get updated metadata")]
    pub(super) machine: MachineId,
    #[clap(long, help = "The updated name of the Machine")]
    pub(super) name: Option<String>,
    #[clap(long, help = "The updated description of the Machine")]
    pub(super) description: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add a key-only label:
    $ nico-admin-cli machine metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge

Add a key/value label:
    $ nico-admin-cli machine metadata add-label 12345678-1234-5678-90ab-cdef01234567 \
    --key rack --value 4

")]
pub(crate) struct MachineMetadataCommandAddLabel {
    #[clap(help = "The machine which should get updated metadata")]
    pub(super) machine: MachineId,
    #[clap(long, help = "The key to add")]
    pub(super) key: String,
    #[clap(long, help = "The optional value to add")]
    pub(super) value: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Remove one or more labels by key:
    $ nico-admin-cli machine metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 \
    --keys rack --keys edge

")]
pub(crate) struct MachineMetadataCommandRemoveLabels {
    #[clap(help = "The machine which should get updated metadata")]
    pub(super) machine: MachineId,
    #[clap(long, help = "The keys to remove")]
    pub(super) keys: Vec<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Fill in missing metadata from the expected-machine (leaving existing values intact):
    $ nico-admin-cli machine metadata from-expected-machine 12345678-1234-5678-90ab-cdef01234567

Overwrite the machine's metadata with the expected-machine's values:
    $ nico-admin-cli machine metadata from-expected-machine 12345678-1234-5678-90ab-cdef01234567 \
    --replace-all

")]
pub(crate) struct MachineMetadataCommandFromExpectedMachine {
    #[clap(help = "The machine which should get updated metadata")]
    pub(super) machine: MachineId,
    /// Whether to fully replace the Metadata that is currently stored on the Machine.
    /// - If not set, existing Metadata on the Machine will not be touched by executing
    ///   the command:
    ///   - The existing Name will not be changed if the Name is not equivalent
    ///     to the Machine ID or Empty.
    ///   - The existing Description will not be changed if it is not empty.
    ///   - Existing Labels and their values will not be changed. Only labels which
    ///     do not exist on the Machine will be added.
    /// - If set, the Machines Metadata will be set to the same values as
    ///   they would if the Machine would get freshly ingested.
    ///   Metadata that is currently set on the Machine will be overridden.
    #[clap(long, verbatim_doc_comment)]
    pub(super) replace_all: bool,
}

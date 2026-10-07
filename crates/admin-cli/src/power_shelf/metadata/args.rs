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

use carbide_uuid::power_shelf::PowerShelfId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set the Name or Description of the Power Shelf")]
    Set(PowerShelfMetadataCommandSet),
    #[clap(about = "Show the Metadata of the Power Shelf")]
    Show(PowerShelfMetadataCommandShow),
    #[clap(about = "Adds a label to the Metadata of a Power Shelf")]
    AddLabel(PowerShelfMetadataCommandAddLabel),
    #[clap(about = "Removes labels from the Metadata of a Power Shelf")]
    RemoveLabels(PowerShelfMetadataCommandRemoveLabels),
    #[clap(about = "Copy Power Shelf Metadata from Expected-Power-Shelf to Power Shelf")]
    FromExpectedPowerShelf(PowerShelfMetadataCommandFromExpectedPowerShelf),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show a power shelf's metadata:
    $ nico-admin-cli power-shelf metadata show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct PowerShelfMetadataCommandShow {
    #[clap(help = "The power shelf which should get its metadata displayed")]
    pub(super) power_shelf: PowerShelfId,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a power shelf's name and description:
    $ nico-admin-cli power-shelf metadata set 12345678-1234-5678-90ab-cdef01234567 \
    --name ps-01 --description \"Rack 4 power shelf\"

")]
pub(crate) struct PowerShelfMetadataCommandSet {
    #[clap(help = "The power shelf which should get updated metadata")]
    pub(super) power_shelf: PowerShelfId,
    #[clap(long, help = "The updated name of the Power Shelf")]
    pub(super) name: Option<String>,
    #[clap(long, help = "The updated description of the Power Shelf")]
    pub(super) description: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add a key-only label:
    $ nico-admin-cli power-shelf metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge

Add a key/value label:
    $ nico-admin-cli power-shelf metadata add-label 12345678-1234-5678-90ab-cdef01234567 \
    --key row --value C

")]
pub(crate) struct PowerShelfMetadataCommandAddLabel {
    #[clap(help = "The power shelf which should get updated metadata")]
    pub(super) power_shelf: PowerShelfId,
    #[clap(long, help = "The key to add")]
    pub(super) key: String,
    #[clap(long, help = "The optional value to add")]
    pub(super) value: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Remove one or more labels by key:
    $ nico-admin-cli power-shelf metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 \
    --keys row --keys edge

")]
pub(crate) struct PowerShelfMetadataCommandRemoveLabels {
    #[clap(help = "The power shelf which should get updated metadata")]
    pub(super) power_shelf: PowerShelfId,
    #[clap(long, help = "The keys to remove")]
    pub(super) keys: Vec<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Fill in missing metadata from the expected-power-shelf (leaving existing values intact):
    $ nico-admin-cli power-shelf metadata from-expected-power-shelf 12345678-1234-5678-90ab-cdef01234567

Overwrite the power shelf's metadata with the expected-power-shelf's values:
    $ nico-admin-cli power-shelf metadata from-expected-power-shelf 12345678-1234-5678-90ab-cdef01234567 \
    --replace-all

")]
pub(crate) struct PowerShelfMetadataCommandFromExpectedPowerShelf {
    #[clap(help = "The power shelf which should get updated metadata")]
    pub(super) power_shelf: PowerShelfId,
    /// Whether to fully replace the Metadata that is currently stored on the Power Shelf.
    /// - If not set, existing Metadata on the Power Shelf will not be touched by executing
    ///   the command:
    ///   - The existing Name will not be changed if the Name is not equivalent
    ///     to the Power Shelf ID or Empty.
    ///   - The existing Description will not be changed if it is not empty.
    ///   - Existing Labels and their values will not be changed. Only labels which
    ///     do not exist on the Power Shelf will be added.
    /// - If set, the Power Shelves Metadata will be set to the same values as
    ///   they would if the Power Shelf would get freshly ingested.
    ///   Metadata that is currently set on the Power Shelf will be overridden.
    #[clap(long, verbatim_doc_comment)]
    pub(super) replace_all: bool,
}

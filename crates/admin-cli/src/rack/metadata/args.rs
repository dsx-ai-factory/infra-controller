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

use carbide_uuid::rack::RackId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set the Name or Description of the Rack")]
    Set(RackMetadataCommandSet),
    #[clap(about = "Show the Metadata of the Rack")]
    Show(RackMetadataCommandShow),
    #[clap(about = "Adds a label to the Metadata of a Rack")]
    AddLabel(RackMetadataCommandAddLabel),
    #[clap(about = "Removes labels from the Metadata of a Rack")]
    RemoveLabels(RackMetadataCommandRemoveLabels),
    #[clap(about = "Copy Rack Metadata from Expected-Rack to Rack")]
    FromExpectedRack(RackMetadataCommandFromExpectedRack),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show a rack's metadata:
    $ nico-admin-cli rack metadata show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct RackMetadataCommandShow {
    #[clap(help = "The rack which should get its metadata displayed")]
    pub(super) rack: RackId,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a rack's name and description:
    $ nico-admin-cli rack metadata set 12345678-1234-5678-90ab-cdef01234567 \
    --name rack-01 --description \"Row C, position 1\"

")]
pub(crate) struct RackMetadataCommandSet {
    #[clap(help = "The rack which should get updated metadata")]
    pub(super) rack: RackId,
    #[clap(long, help = "The updated name of the Rack")]
    pub(super) name: Option<String>,
    #[clap(long, help = "The updated description of the Rack")]
    pub(super) description: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add a key-only label:
    $ nico-admin-cli rack metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge

Add a key/value label:
    $ nico-admin-cli rack metadata add-label 12345678-1234-5678-90ab-cdef01234567 \
    --key row --value C

")]
pub(crate) struct RackMetadataCommandAddLabel {
    #[clap(help = "The rack which should get updated metadata")]
    pub(super) rack: RackId,
    #[clap(long, help = "The key to add")]
    pub(super) key: String,
    #[clap(long, help = "The optional value to add")]
    pub(super) value: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Remove one or more labels by key:
    $ nico-admin-cli rack metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 \
    --keys row --keys edge

")]
pub(crate) struct RackMetadataCommandRemoveLabels {
    #[clap(help = "The rack which should get updated metadata")]
    pub(super) rack: RackId,
    #[clap(long, help = "The keys to remove")]
    pub(super) keys: Vec<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Fill in missing metadata from the expected-rack (leaving existing values intact):
    $ nico-admin-cli rack metadata from-expected-rack 12345678-1234-5678-90ab-cdef01234567

Overwrite the rack's metadata with the expected-rack's values:
    $ nico-admin-cli rack metadata from-expected-rack 12345678-1234-5678-90ab-cdef01234567 \
    --replace-all

")]
pub(crate) struct RackMetadataCommandFromExpectedRack {
    #[clap(help = "The rack which should get updated metadata")]
    pub(super) rack: RackId,
    /// Whether to fully replace the Metadata that is currently stored on the Rack.
    /// - If not set, existing Metadata on the Rack will not be touched by executing
    ///   the command:
    ///   - The existing Name will not be changed if the Name is not equivalent
    ///     to the Rack ID or Empty.
    ///   - The existing Description will not be changed if it is not empty.
    ///   - Existing Labels and their values will not be changed. Only labels which
    ///     do not exist on the Rack will be added.
    /// - If set, the Racks Metadata will be set to the same values as
    ///   they would if the Rack would get freshly ingested.
    ///   Metadata that is currently set on the Rack will be overridden.
    #[clap(long, verbatim_doc_comment)]
    pub(super) replace_all: bool,
}

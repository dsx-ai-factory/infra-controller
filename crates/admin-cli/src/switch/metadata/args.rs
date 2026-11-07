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

use carbide_uuid::switch::SwitchId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
pub(crate) enum Args {
    #[clap(about = "Set the Name or Description of the Switch")]
    Set(SwitchMetadataCommandSet),
    #[clap(about = "Show the Metadata of the Switch")]
    Show(SwitchMetadataCommandShow),
    #[clap(about = "Adds a label to the Metadata of a Switch")]
    AddLabel(SwitchMetadataCommandAddLabel),
    #[clap(about = "Removes labels from the Metadata of a Switch")]
    RemoveLabels(SwitchMetadataCommandRemoveLabels),
    #[clap(about = "Copy Switch Metadata from Expected-Switch to Switch")]
    FromExpectedSwitch(SwitchMetadataCommandFromExpectedSwitch),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show a switch's metadata:
    $ nico-admin-cli switch metadata show 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct SwitchMetadataCommandShow {
    #[clap(help = "The switch which should get its metadata displayed")]
    pub(super) switch: SwitchId,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a switch's name and description:
    $ nico-admin-cli switch metadata set 12345678-1234-5678-90ab-cdef01234567 \
    --name spine-01 --description \"Rack 4 spine\"

")]
pub(crate) struct SwitchMetadataCommandSet {
    #[clap(help = "The switch which should get updated metadata")]
    pub(super) switch: SwitchId,
    #[clap(long, help = "The updated name of the Switch")]
    pub(super) name: Option<String>,
    #[clap(long, help = "The updated description of the Switch")]
    pub(super) description: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Add a key-only label:
    $ nico-admin-cli switch metadata add-label 12345678-1234-5678-90ab-cdef01234567 --key edge

Add a key/value label:
    $ nico-admin-cli switch metadata add-label 12345678-1234-5678-90ab-cdef01234567 \
    --key env --value prod

")]
pub(crate) struct SwitchMetadataCommandAddLabel {
    #[clap(help = "The switch which should get updated metadata")]
    pub(super) switch: SwitchId,
    #[clap(long, help = "The key to add")]
    pub(super) key: String,
    #[clap(long, help = "The optional value to add")]
    pub(super) value: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Remove one or more labels by key:
    $ nico-admin-cli switch metadata remove-labels 12345678-1234-5678-90ab-cdef01234567 \
    --keys env --keys edge

")]
pub(crate) struct SwitchMetadataCommandRemoveLabels {
    #[clap(help = "The switch which should get updated metadata")]
    pub(super) switch: SwitchId,
    #[clap(long, help = "The keys to remove")]
    pub(super) keys: Vec<String>,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Fill in missing metadata from the expected-switch (leaving existing values intact):
    $ nico-admin-cli switch metadata from-expected-switch 12345678-1234-5678-90ab-cdef01234567

Overwrite the switch's metadata with the expected-switch's values:
    $ nico-admin-cli switch metadata from-expected-switch 12345678-1234-5678-90ab-cdef01234567 \
    --replace-all

")]
pub(crate) struct SwitchMetadataCommandFromExpectedSwitch {
    #[clap(help = "The switch which should get updated metadata")]
    pub(super) switch: SwitchId,
    /// Whether to fully replace the Metadata that is currently stored on the Switch.
    /// - If not set, existing Metadata on the Switch will not be touched by executing
    ///   the command:
    ///   - The existing Name will not be changed if the Name is not equivalent
    ///     to the Switch ID or Empty.
    ///   - The existing Description will not be changed if it is not empty.
    ///   - Existing Labels and their values will not be changed. Only labels which
    ///     do not exist on the Switch will be added.
    /// - If set, the Switches Metadata will be set to the same values as
    ///   they would if the Switch would get freshly ingested.
    ///   Metadata that is currently set on the Switch will be overridden.
    #[clap(long, verbatim_doc_comment)]
    pub(super) replace_all: bool,
}

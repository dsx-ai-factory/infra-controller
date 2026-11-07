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

/*!
 *  Measured Boot CLI arguments for the `measurement bundle` subcommand.
 *
 * This provides the CLI subcommands and arguments for:
 *  - `bundle create`: Create a new measurement bundle.
 *  - `bundle delete`: Delete an existing measurement bundle.
 *  - `bundle rename`: Rename an existing measurement bundle.
 *  - `bundle set-state`: Change the state of a measurement bundle.
 *  - `bundle show`: Show all details about measurement bundle(s).
 *  - `bundle list all`: List high level metadata about all bundles.
 *  - `bundle list machines`: List all matchines matching a given bundle.
 */

use std::str::FromStr;

use ::rpc::protos::measured_boot::{
    CreateMeasurementBundleRequest, DeleteMeasurementBundleRequest, FindClosestBundleMatchRequest,
    ListMeasurementBundleMachinesRequest, MeasurementBundleStatePb, RenameMeasurementBundleRequest,
    ShowMeasurementBundleRequest, UpdateMeasurementBundleRequest,
    delete_measurement_bundle_request, list_measurement_bundle_machines_request,
    rename_measurement_bundle_request, show_measurement_bundle_request,
    update_measurement_bundle_request,
};
use carbide_uuid::measured_boot::{
    MeasurementBundleId, MeasurementReportId, MeasurementSystemProfileId,
};
use clap::Parser;
use measured_boot::pcr::PcrRegisterValue;
use measured_boot::records::MeasurementBundleState;

use crate::attestation::measured_boot::global::cmds::{
    IdNameIdentifier, IdentifierType, get_identifier,
};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::measurement::parse_pcr_register_values;
use crate::errors::CarbideCliError;

/// CmdBundle provides a container for the `bundle` subcommand, which itself
/// contains other subcommands for working with profiles.
#[derive(Parser, Debug, Dispatch)]
pub(crate) enum CmdBundle {
    #[clap(
        about = "Create a new bundle with a given values, for a given profile ID.",
        visible_alias = "c"
    )]
    Create(Create),

    #[clap(about = "Delete a bundle based on ID", visible_alias = "d")]
    Delete(Delete),

    #[clap(about = "Rename a bundle.", visible_alias = "r")]
    Rename(Rename),

    #[clap(about = "Set a new state for a bundle.", visible_alias = "u")]
    SetState(SetState),

    #[clap(about = "Show a bundle (or all).", visible_alias = "s")]
    Show(Show),

    #[clap(
        subcommand,
        about = "Get closest bundle to a report.",
        visible_alias = "g"
    )]
    #[dispatch]
    FindClosestMatch(FindClosestMatch),

    #[clap(
        subcommand,
        about = "List bundles by various ways.",
        visible_alias = "l"
    )]
    #[dispatch]
    List(List),
}

/// Create is used to create a new bundle, associated with a given profile ID
/// or profile name, with provided PCR values and an optional
/// MeasurementBundleState (the default is 'active').
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create an active bundle for a profile with two PCR values:
    $ nico-admin-cli attestation measured-boot bundle create my-bundle \
    12345678-1234-5678-90ab-cdef01234567 0:abc123,7:def456

Create a bundle in the pending state (awaiting approval):
    $ nico-admin-cli attestation measured-boot bundle create my-bundle \
    12345678-1234-5678-90ab-cdef01234567 0:abc123 --state pending

")]
pub(crate) struct Create {
    #[clap(help = "A human-readable name to give this bundle.")]
    name: String,

    #[clap(help = "The profile ID of the profile to associate this bundle with.")]
    profile_id: MeasurementSystemProfileId,

    #[clap(
        required = true,
        use_value_delimiter = true,
        value_delimiter = ',',
        help = "Comma-separated list of {pcr_register:value,...} to associate with this bundle."
    )]
    #[arg(value_parser = parse_pcr_register_values)]
    values: Vec<PcrRegisterValue>,

    // state is optional, and if unset, the database itself
    // is configured to default to 'active'.
    #[clap(
        long,
        value_enum,
        help = "The state for this bundle (default: active)."
    )]
    state: Option<MeasurementBundleState>,
}

/// Delete will delete a bundle for the given ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Delete a bundle by ID:
    $ nico-admin-cli attestation measured-boot bundle delete \
    12345678-1234-5678-90ab-cdef01234567

Delete a bundle and purge its journal records:
    $ nico-admin-cli attestation measured-boot bundle delete \
    12345678-1234-5678-90ab-cdef01234567 --purge-journals

")]
pub(crate) struct Delete {
    #[clap(help = "The bundle ID.")]
    bundle_id: MeasurementBundleId,

    #[clap(long, help = "Also purge any journal records for this bundle.")]
    purge_journals: bool,
}

/// Rename will rename a bundle for the given ID or name.
/// A parser will parse the `identifier` to determine if
/// the API should be called w/ an ID or name selector.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Rename a bundle, letting the CLI detect whether the identifier is an ID or name:
    $ nico-admin-cli attestation measured-boot bundle rename old-name new-name

Rename a bundle selected explicitly by ID:
    $ nico-admin-cli attestation measured-boot bundle rename \
    12345678-1234-5678-90ab-cdef01234567 new-name --is-id

")]
pub(crate) struct Rename {
    #[clap(help = "The existing bundle ID or name.")]
    identifier: String,

    #[clap(help = "The new bundle name.")]
    new_bundle_name: String,

    #[clap(long, help = "Explicitly say the identifier is bundle ID.")]
    is_id: bool,

    #[clap(long, help = "Explicitly say the identifier is a bundle name.")]
    is_name: bool,
}

impl IdNameIdentifier for Rename {
    fn is_id(&self) -> bool {
        self.is_id
    }

    fn is_name(&self) -> bool {
        self.is_name
    }
}

/// Show will get + display a bundle for the given ID, or, if not ID is set,
/// it will display all bundles and their information.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show all bundles:
    $ nico-admin-cli attestation measured-boot bundle show

Show one bundle by ID:
    $ nico-admin-cli attestation measured-boot bundle show \
    12345678-1234-5678-90ab-cdef01234567 --is-id

Show one bundle by name:
    $ nico-admin-cli attestation measured-boot bundle show my-bundle --is-name

")]
pub(crate) struct Show {
    #[clap(help = "The optional bundle ID or name.")]
    pub(super) identifier: Option<String>,

    #[clap(long, help = "Explicitly say the identifier is bundle ID.")]
    is_id: bool,

    #[clap(long, help = "Explicitly say the identifier is a bundle name.")]
    is_name: bool,
}

impl IdNameIdentifier for Show {
    fn is_id(&self) -> bool {
        self.is_id
    }

    fn is_name(&self) -> bool {
        self.is_name
    }
}

/// SetState is used to set the state of the bundle (e.g. active, obsolete,
/// retired, revoked).
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Mark a bundle obsolete (selected by name):
    $ nico-admin-cli attestation measured-boot bundle set-state my-bundle obsolete

Revoke a known-bad bundle by ID:
    $ nico-admin-cli attestation measured-boot bundle set-state \
    12345678-1234-5678-90ab-cdef01234567 revoked --is-id

")]
pub(crate) struct SetState {
    #[clap(help = "The bundle ID or name to update.")]
    identifier: String,

    #[clap(
        required = true,
        value_enum,
        help = "The state to set for this bundle."
    )]
    state: MeasurementBundleState,

    #[clap(long, help = "Explicitly say the identifier is bundle ID.")]
    is_id: bool,

    #[clap(long, help = "Explicitly say the identifier is a bundle name.")]
    is_name: bool,
}

impl IdNameIdentifier for SetState {
    fn is_id(&self) -> bool {
        self.is_id
    }

    fn is_name(&self) -> bool {
        self.is_name
    }
}

/// List provides a few ways to list things.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List metadata for all bundles:
    $ nico-admin-cli attestation measured-boot bundle list all

List all machines matching a bundle:
    $ nico-admin-cli attestation measured-boot bundle list machines my-bundle

")]
pub(crate) enum List {
    #[clap(about = "List all bundles", visible_alias = "a")]
    All(ListAll),

    #[clap(
        about = "List all machines for a given bundle ID.",
        visible_alias = "m"
    )]
    Machines(ListMachines),
}

/// ListAll will list all bundles.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List metadata for all bundles:
    $ nico-admin-cli attestation measured-boot bundle list all

")]
pub(crate) struct ListAll {}

/// ListMachines lists all machines for a given bundle (by bundle name or ID).
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all machines matching a bundle by name:
    $ nico-admin-cli attestation measured-boot bundle list machines my-bundle

List all machines matching a bundle selected explicitly by ID:
    $ nico-admin-cli attestation measured-boot bundle list machines \
    12345678-1234-5678-90ab-cdef01234567 --is-id

")]
pub(crate) struct ListMachines {
    #[clap(help = "The existing bundle ID or name.")]
    identifier: String,

    #[clap(long, help = "Explicitly say the identifier is bundle ID.")]
    is_id: bool,

    #[clap(long, help = "Explicitly say the identifier is a bundle name.")]
    is_name: bool,
}

impl IdNameIdentifier for ListMachines {
    fn is_id(&self) -> bool {
        self.is_id
    }

    fn is_name(&self) -> bool {
        self.is_name
    }
}

#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Find the closest matching bundle for a report:
    $ nico-admin-cli attestation measured-boot bundle find-closest-match report \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum FindClosestMatch {
    #[clap(about = "The existing report ID.")]
    Report(ReportId),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Find the closest matching bundle for a report:
    $ nico-admin-cli attestation measured-boot bundle find-closest-match report \
    12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ReportId {
    #[clap(help = "Report ID.")]
    id: MeasurementReportId,
}

impl From<Create> for CreateMeasurementBundleRequest {
    fn from(create: Create) -> Self {
        let state: MeasurementBundleStatePb = match create.state {
            Some(input_state) => input_state.into(),
            None => MeasurementBundleStatePb::Active,
        };
        Self {
            name: Some(create.name),
            profile_id: Some(create.profile_id),
            pcr_values: create.values.into_iter().map(Into::into).collect(),
            state: state.into(),
        }
    }
}

impl From<Delete> for DeleteMeasurementBundleRequest {
    fn from(delete: Delete) -> Self {
        Self {
            selector: Some(delete_measurement_bundle_request::Selector::BundleId(
                delete.bundle_id,
            )),
        }
    }
}

impl TryFrom<Rename> for RenameMeasurementBundleRequest {
    type Error = CarbideCliError;
    fn try_from(rename: Rename) -> Result<Self, Self::Error> {
        let selector = match get_identifier(&rename)? {
            IdentifierType::ForId => {
                let bundle_id = MeasurementBundleId::from_str(&rename.identifier)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
                Some(rename_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                ))
            }
            IdentifierType::ForName => Some(
                rename_measurement_bundle_request::Selector::BundleName(rename.identifier),
            ),
            IdentifierType::Detect => match MeasurementBundleId::from_str(&rename.identifier) {
                Ok(bundle_id) => Some(rename_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                )),
                Err(_) => Some(rename_measurement_bundle_request::Selector::BundleName(
                    rename.identifier,
                )),
            },
        };
        Ok(Self {
            new_bundle_name: rename.new_bundle_name,
            selector,
        })
    }
}

impl TryFrom<SetState> for UpdateMeasurementBundleRequest {
    type Error = CarbideCliError;
    fn try_from(set_state: SetState) -> Result<Self, Self::Error> {
        let state: MeasurementBundleStatePb = set_state.state.into();
        let selector = match get_identifier(&set_state)? {
            IdentifierType::ForId => {
                let bundle_id = MeasurementBundleId::from_str(&set_state.identifier)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
                Some(update_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                ))
            }
            IdentifierType::ForName => Some(
                update_measurement_bundle_request::Selector::BundleName(set_state.identifier),
            ),
            IdentifierType::Detect => match MeasurementBundleId::from_str(&set_state.identifier) {
                Ok(bundle_id) => Some(update_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                )),
                Err(_) => Some(update_measurement_bundle_request::Selector::BundleName(
                    set_state.identifier,
                )),
            },
        };
        Ok(Self {
            state: state.into(),
            selector,
        })
    }
}

impl TryFrom<Show> for ShowMeasurementBundleRequest {
    type Error = CarbideCliError;
    fn try_from(show: Show) -> Result<Self, Self::Error> {
        let identifier_type = get_identifier(&show)?;
        let identifier = show
            .identifier
            .ok_or(CarbideCliError::GenericError(String::from(
                "identifier expected to be set here",
            )))?;
        let selector = match identifier_type {
            IdentifierType::ForId => {
                let bundle_id = MeasurementBundleId::from_str(&identifier)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
                Some(show_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                ))
            }
            IdentifierType::ForName => Some(show_measurement_bundle_request::Selector::BundleName(
                identifier,
            )),
            IdentifierType::Detect => match MeasurementBundleId::from_str(&identifier) {
                Ok(bundle_id) => Some(show_measurement_bundle_request::Selector::BundleId(
                    bundle_id,
                )),
                Err(_) => Some(show_measurement_bundle_request::Selector::BundleName(
                    identifier,
                )),
            },
        };
        Ok(Self { selector })
    }
}

impl TryFrom<ListMachines> for ListMeasurementBundleMachinesRequest {
    type Error = CarbideCliError;
    fn try_from(list_machines: ListMachines) -> Result<Self, Self::Error> {
        let selector = match get_identifier(&list_machines)? {
            IdentifierType::ForId => {
                let bundle_id = MeasurementBundleId::from_str(&list_machines.identifier)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;
                Some(list_measurement_bundle_machines_request::Selector::BundleId(bundle_id))
            }
            IdentifierType::ForName => Some(
                list_measurement_bundle_machines_request::Selector::BundleName(
                    list_machines.identifier,
                ),
            ),
            IdentifierType::Detect => {
                match MeasurementBundleId::from_str(&list_machines.identifier) {
                    Ok(bundle_id) => Some(
                        list_measurement_bundle_machines_request::Selector::BundleId(bundle_id),
                    ),
                    Err(_) => Some(
                        list_measurement_bundle_machines_request::Selector::BundleName(
                            list_machines.identifier,
                        ),
                    ),
                }
            }
        };
        Ok(Self { selector })
    }
}

impl From<FindClosestMatch> for FindClosestBundleMatchRequest {
    fn from(args: FindClosestMatch) -> Self {
        match args {
            FindClosestMatch::Report(report_id) => Self {
                report_id: Some(report_id.id),
            },
        }
    }
}

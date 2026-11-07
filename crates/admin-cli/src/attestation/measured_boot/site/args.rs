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
 *  Measured Boot CLI arguments for the `measurement site` subcommand.
 *
 * This provides the CLI subcommands and arguments for:
 *  - `site import`: Import a site model (profiles + bundles) from a file.
 *  - `site export`: Export a site model from DB -> to a file.
 *  - `site trusted-machine approve`: Create a trusted machine approval.
 *  - `site trusted-machine remove`: Remove a trusted machine approval.
 *  - `site trusted-machine list`: List all trusted machine approvals.
 *  - `site trusted-profile approve`: Create a trusted profile approval.
 *  - `site trusted-profile remove`: Remove a trusted profile approval.
 *  - `site trusted-profile list`: List all trusted profile approvals.
 */

use ::rpc::protos::measured_boot::{
    AddMeasurementTrustedMachineRequest, AddMeasurementTrustedProfileRequest,
    MeasurementApprovedTypePb, RemoveMeasurementTrustedMachineRequest,
    RemoveMeasurementTrustedProfileRequest, remove_measurement_trusted_machine_request,
    remove_measurement_trusted_profile_request,
};
use carbide_uuid::measured_boot::{
    MeasurementApprovedMachineId, MeasurementApprovedProfileId, MeasurementSystemProfileId,
    TrustedMachineId,
};
use clap::Parser;
use measured_boot::records::MeasurementApprovedType;

use crate::cfg::dispatch::Dispatch;

/// CmdSite provides a container for the `site` subcommand, which itself
/// contains other subcommands for working with the site (i.e. export
/// and import).
#[derive(Parser, Debug, Dispatch)]
pub(crate) enum CmdSite {
    #[clap(about = "Import a site from an export file.", visible_alias = "i")]
    Import(Import),

    #[clap(about = "Export a site to an export file.", visible_alias = "e")]
    Export(Export),

    #[clap(subcommand, about = "Managed trusted machines.", visible_alias = "m")]
    #[dispatch]
    TrustedMachine(TrustedMachine),

    #[clap(subcommand, about = "Managed trusted profiles.", visible_alias = "p")]
    #[dispatch]
    TrustedProfile(TrustedProfile),
}

/// Import imports a site from a file.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Import a site model (profiles + bundles) from a JSON file:
    $ nico-admin-cli attestation measured-boot site import ./site.json

")]
pub(crate) struct Import {
    #[clap(help = "The path of the input JSON file.")]
    pub(super) path: String,
}

/// Export exports a site to stdout, a file, etc.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Export the site model to stdout:
    $ nico-admin-cli attestation measured-boot site export

Export the site model to a file:
    $ nico-admin-cli attestation measured-boot site export --path ./site.json

")]
pub(crate) struct Export {
    #[clap(long, help = "An optional path to write the file to.")]
    pub(super) path: Option<String>,
}

/// TrustedMachine configures trusted machine settings.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Approve a machine for one-shot auto-promotion of its measurements:
    $ nico-admin-cli attestation measured-boot site trusted-machine approve \
    12345678-1234-5678-90ab-cdef01234567 oneshot

List all active machine approvals:
    $ nico-admin-cli attestation measured-boot site trusted-machine list

Remove an approval by machine ID:
    $ nico-admin-cli attestation measured-boot site trusted-machine remove \
    by-machine-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum TrustedMachine {
    #[clap(
        about = "Approve a trusted machine for auto-promoting its measurements.",
        visible_alias = "a"
    )]
    Approve(ApproveMachine),

    #[clap(
        subcommand,
        about = "Remove a trusted machine approval.",
        visible_alias = "r"
    )]
    #[dispatch]
    Remove(RemoveMachine),

    #[clap(about = "List all active machine approvals.", visible_alias = "l")]
    List(ListMachines),
}

/// TrustedProfile configures trusted profile settings.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Approve a profile for persistent auto-promotion of matching machines' measurements:
    $ nico-admin-cli attestation measured-boot site trusted-profile approve \
    12345678-1234-5678-90ab-cdef01234567 persist

List all active profile approvals:
    $ nico-admin-cli attestation measured-boot site trusted-profile list

Remove an approval by profile ID:
    $ nico-admin-cli attestation measured-boot site trusted-profile remove \
    by-profile-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum TrustedProfile {
    #[clap(
        about = "Allow auto-promoting of measurements from machines matching a profile.",
        visible_alias = "a"
    )]
    Approve(ApproveProfile),

    #[clap(
        subcommand,
        about = "Remove a trusted profile approval.",
        visible_alias = "r"
    )]
    #[dispatch]
    Remove(RemoveProfile),

    #[clap(about = "List all active profile approvals.", visible_alias = "l")]
    List(ListProfiles),
}

/// ApproveMachine approves a machine for auto-promoting its measurement
/// journal entries into a golden measurement bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Approve a single machine for one-shot auto-promotion:
    $ nico-admin-cli attestation measured-boot site trusted-machine approve \
    12345678-1234-5678-90ab-cdef01234567 oneshot

Approve all machines persistently, restricted to specific PCR registers, with a comment:
    $ nico-admin-cli attestation measured-boot site trusted-machine approve '*' \
    persist --pcr-registers 0,7 --comments \"trusted fleet\"

")]
pub(crate) struct ApproveMachine {
    #[clap(help = "The machine-id to approve (or '*' for all).")]
    machine_id: TrustedMachineId,

    #[clap(required = true, help = "Whether to set `oneshot` or `persist`.")]
    approval_type: MeasurementApprovedType,

    #[clap(long, help = "Specific PCR register selector. All if unset.")]
    pcr_registers: Option<String>,

    #[clap(long, help = "Optional comments about this approval.")]
    comments: Option<String>,
}

/// RemoveMachine removes a machine from auto-approval, by approval ID
/// or machine ID.
//
// The compiler yells it starts by "By", not really
// understanding that its a part of the CLI UX.
#[allow(clippy::enum_variant_names)]
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Remove a machine approval by approval ID:
    $ nico-admin-cli attestation measured-boot site trusted-machine remove \
    by-approval-id 12345678-1234-5678-90ab-cdef01234567

Remove a machine approval by machine ID:
    $ nico-admin-cli attestation measured-boot site trusted-machine remove \
    by-machine-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum RemoveMachine {
    #[clap(about = "Remove by approval ID.")]
    ByApprovalId(RemoveMachineByApprovalId),

    #[clap(about = "Remove by machine ID.")]
    ByMachineId(RemoveMachineByMachineId),
}

/// RemoveMachineByApprovalId removes a trusted machine approval
/// for the given approval ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Remove a machine approval by approval ID:
    $ nico-admin-cli attestation measured-boot site trusted-machine remove \
    by-approval-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct RemoveMachineByApprovalId {
    #[clap(help = "The approval-id to remove.")]
    approval_id: MeasurementApprovedMachineId,
}

/// RemoveMachineByMachineId removes a trusted machine approval
/// for the given machine ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Remove a machine approval by machine ID:
    $ nico-admin-cli attestation measured-boot site trusted-machine remove \
    by-machine-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct RemoveMachineByMachineId {
    #[clap(help = "The machine-id to remove.")]
    machine_id: TrustedMachineId,
}

/// ListMachines is used to list all active machine approvals.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all active machine approvals:
    $ nico-admin-cli attestation measured-boot site trusted-machine list

")]
pub(crate) struct ListMachines {}

/// ApproveProfile approves a profile for auto-promoting its
/// measurement journal entries into a golden measurement bundle.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Approve a profile for one-shot auto-promotion:
    $ nico-admin-cli attestation measured-boot site trusted-profile approve \
    12345678-1234-5678-90ab-cdef01234567 oneshot

Approve a profile persistently, restricted to specific PCR registers, with a comment:
    $ nico-admin-cli attestation measured-boot site trusted-profile approve \
    12345678-1234-5678-90ab-cdef01234567 persist --pcr-registers 0,7 --comments \"trusted SKU\"

")]
pub(crate) struct ApproveProfile {
    #[clap(help = "The profile-id to approve.")]
    profile_id: MeasurementSystemProfileId,

    #[clap(required = true, help = "Whether to set `oneshot` or `persist`.")]
    approval_type: MeasurementApprovedType,

    #[clap(long, help = "Specific PCR register selector. All if unset.")]
    pcr_registers: Option<String>,

    #[clap(long, help = "Optional comments about this approval.")]
    comments: Option<String>,
}

/// RemoveProfile removes a machine from auto-approval, by approval ID
/// or profile ID.
//
// The compiler yells it starts by "By", not really
// understanding that its a part of the CLI UX.
#[allow(clippy::enum_variant_names)]
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Remove a profile approval by approval ID:
    $ nico-admin-cli attestation measured-boot site trusted-profile remove \
    by-approval-id 12345678-1234-5678-90ab-cdef01234567

Remove a profile approval by profile ID:
    $ nico-admin-cli attestation measured-boot site trusted-profile remove \
    by-profile-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum RemoveProfile {
    #[clap(about = "Remove by approval ID.")]
    ByApprovalId(RemoveProfileByApprovalId),

    #[clap(about = "Remove by profile ID.")]
    ByProfileId(RemoveProfileByProfileId),
}

/// RemoveProfileByApprovalId removes a trusted profile approval
/// for the given approval ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Remove a profile approval by approval ID:
    $ nico-admin-cli attestation measured-boot site trusted-profile remove \
    by-approval-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct RemoveProfileByApprovalId {
    #[clap(help = "The approval-id to remove.")]
    approval_id: MeasurementApprovedProfileId,
}

/// RemoveProfileByProfileId removes a trusted profile approval
/// for the given profile ID.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Remove a profile approval by profile ID:
    $ nico-admin-cli attestation measured-boot site trusted-profile remove \
    by-profile-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct RemoveProfileByProfileId {
    #[clap(help = "The profile-id to remove.")]
    profile_id: MeasurementSystemProfileId,
}

/// ListProfiles is used to list all active profile approvals.
#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all active profile approvals:
    $ nico-admin-cli attestation measured-boot site trusted-profile list

")]
pub(crate) struct ListProfiles {}

impl From<ApproveMachine> for AddMeasurementTrustedMachineRequest {
    fn from(approve: ApproveMachine) -> Self {
        let approval_type: MeasurementApprovedTypePb = approve.approval_type.into();
        Self {
            machine_id: approve.machine_id.to_string(),
            approval_type: approval_type.into(),
            pcr_registers: approve.pcr_registers.unwrap_or_default(),
            comments: approve.comments.unwrap_or_default(),
        }
    }
}

impl From<RemoveMachineByApprovalId> for RemoveMeasurementTrustedMachineRequest {
    fn from(by_approval_id: RemoveMachineByApprovalId) -> Self {
        Self {
            selector: Some(
                remove_measurement_trusted_machine_request::Selector::ApprovalId(
                    by_approval_id.approval_id,
                ),
            ),
        }
    }
}

impl From<RemoveMachineByMachineId> for RemoveMeasurementTrustedMachineRequest {
    fn from(by_machine_id: RemoveMachineByMachineId) -> Self {
        Self {
            selector: Some(
                remove_measurement_trusted_machine_request::Selector::MachineId(
                    by_machine_id.machine_id.to_string(),
                ),
            ),
        }
    }
}

impl From<ApproveProfile> for AddMeasurementTrustedProfileRequest {
    fn from(approve: ApproveProfile) -> Self {
        let approval_type: MeasurementApprovedTypePb = approve.approval_type.into();
        Self {
            profile_id: Some(approve.profile_id),
            approval_type: approval_type.into(),
            pcr_registers: approve.pcr_registers,
            comments: approve.comments,
        }
    }
}

impl From<RemoveProfileByApprovalId> for RemoveMeasurementTrustedProfileRequest {
    fn from(by_approval_id: RemoveProfileByApprovalId) -> Self {
        Self {
            selector: Some(
                remove_measurement_trusted_profile_request::Selector::ApprovalId(
                    by_approval_id.approval_id,
                ),
            ),
        }
    }
}

impl From<RemoveProfileByProfileId> for RemoveMeasurementTrustedProfileRequest {
    fn from(by_profile_id: RemoveProfileByProfileId) -> Self {
        Self {
            selector: Some(
                remove_measurement_trusted_profile_request::Selector::ProfileId(
                    by_profile_id.profile_id,
                ),
            ),
        }
    }
}

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

//!
//! `measurement profile` subcommand dispatcher + backing functions.

use std::str::FromStr;

use ::rpc::measured_boot::FromGrpcOpt;
use ::rpc::protos::measured_boot::{
    DeleteMeasurementSystemProfileRequest, ListMeasurementSystemProfileBundlesRequest,
    ListMeasurementSystemProfileMachinesRequest, RenameMeasurementSystemProfileRequest,
    ShowMeasurementSystemProfileRequest,
};
use carbide_uuid::machine::MachineId;
use carbide_uuid::measured_boot::MeasurementBundleId;
use measured_boot::ToTable;
use measured_boot::profile::MeasurementSystemProfile;
use measured_boot::records::MeasurementSystemProfileRecord;
use serde::Serialize;

use crate::attestation::measured_boot::MachineIdList;
use crate::attestation::measured_boot::profile::args::{
    Create, Delete, ListAll, ListBundles, ListMachines, Rename, Show,
};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

impl Run for Create {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            create(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Delete {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            delete(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Rename {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            rename(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Show {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if self.identifier.is_some() {
            crate::cli_output(
                show_by_id_or_name(&ctx.api_client, self).await?,
                &ctx.config.format,
                crate::Destination::Stdout(),
            )
        } else {
            crate::cli_output(
                show_all(&ctx.api_client).await?,
                &ctx.config.format,
                crate::Destination::Stdout(),
            )
        }
    }
}

impl Run for ListAll {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_all(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListBundles {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_bundles_for_id_or_name(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListMachines {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_machines_for_id_or_name(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// create is `profile create` and used for creating
/// a new profile.
async fn create(
    grpc_conn: &ApiClient,
    create: Create,
) -> CarbideCliResult<MeasurementSystemProfile> {
    let response = grpc_conn
        .0
        .create_measurement_system_profile(create)
        .await?;

    MeasurementSystemProfile::from_grpc_opt(response.system_profile)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// delete is `delete <profile-id|profile-name>` and is used
/// for deleting an existing profile by ID or name.
async fn delete(
    grpc_conn: &ApiClient,
    delete: Delete,
) -> CarbideCliResult<MeasurementSystemProfile> {
    let response = grpc_conn
        .0
        .delete_measurement_system_profile(DeleteMeasurementSystemProfileRequest::try_from(delete)?)
        .await?;

    MeasurementSystemProfile::from_grpc_opt(response.system_profile)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// rename renames a measurement bundle with the provided name or ID.
async fn rename(
    grpc_conn: &ApiClient,
    rename: Rename,
) -> CarbideCliResult<MeasurementSystemProfile> {
    let response = grpc_conn
        .0
        .rename_measurement_system_profile(RenameMeasurementSystemProfileRequest::try_from(rename)?)
        .await?;

    MeasurementSystemProfile::from_grpc_opt(response.profile)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// show_all is `show`, and is used for showing all
/// profiles with details (when no <profile_id> is
/// specified on the command line).
async fn show_all(grpc_conn: &ApiClient) -> CarbideCliResult<MeasurementSystemProfileList> {
    Ok(MeasurementSystemProfileList(
        grpc_conn
            .0
            .show_measurement_system_profiles()
            .await?
            .system_profiles
            .into_iter()
            .map(|system_profile| {
                MeasurementSystemProfile::try_from(system_profile)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementSystemProfile>>>()?,
    ))
}

/// show_by_id_or_name is `show <profile-id|profile-name>` and is used for
/// showing a profile (and its details) by ID or name.
async fn show_by_id_or_name(
    grpc_conn: &ApiClient,
    show: Show,
) -> CarbideCliResult<MeasurementSystemProfile> {
    let response = grpc_conn
        .0
        .show_measurement_system_profile(ShowMeasurementSystemProfileRequest::try_from(show)?)
        .await?;

    MeasurementSystemProfile::from_grpc_opt(response.system_profile)
        .map_err(|e| CarbideCliError::GenericError(e.to_string()))
}

/// list_all is `list all` and is used for listing all
/// high level profile info (just IDs). For actual
/// details, use `show`.
async fn list_all(grpc_conn: &ApiClient) -> CarbideCliResult<MeasurementSystemProfileRecordList> {
    Ok(MeasurementSystemProfileRecordList(
        grpc_conn
            .0
            .list_measurement_system_profiles()
            .await?
            .system_profiles
            .into_iter()
            .map(|rec| {
                MeasurementSystemProfileRecord::try_from(rec)
                    .map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementSystemProfileRecord>>>()?,
    ))
}

/// list_bundles_by_id_or_name is `list bundles <profile-id|profile-name>` and
/// is used to list all configured bundles for a given profile ID or name.
async fn list_bundles_for_id_or_name(
    grpc_conn: &ApiClient,
    list_bundles: ListBundles,
) -> CarbideCliResult<MeasurementBundleIdList> {
    Ok(MeasurementBundleIdList(
        grpc_conn
            .0
            .list_measurement_system_profile_bundles(
                ListMeasurementSystemProfileBundlesRequest::try_from(list_bundles)?,
            )
            .await?
            .bundle_ids,
    ))
}

/// list_machines_for_id_or_name is `list machines <profile-id|profile-name>`
/// and is used to list all configured machines associated with a given profile
/// ID or name.
async fn list_machines_for_id_or_name(
    grpc_conn: &ApiClient,
    list_machines: ListMachines,
) -> CarbideCliResult<MachineIdList> {
    Ok(MachineIdList(
        grpc_conn
            .0
            .list_measurement_system_profile_machines(
                ListMeasurementSystemProfileMachinesRequest::try_from(list_machines)?,
            )
            .await?
            .machine_ids
            .iter()
            .map(|rec| {
                MachineId::from_str(rec).map_err(|e| CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MachineId>>>()?,
    ))
}

/// MeasurementSystemProfileRecordList just implements a newtype pattern
/// for a Vec<MeasurementSystemProfileRecord> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementSystemProfileRecordList(Vec<MeasurementSystemProfileRecord>);

impl ToTable for MeasurementSystemProfileRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["profile_id", "name", "created_ts"]);
        for profile in self.0.iter() {
            table.add_row(prettytable::row![
                profile.profile_id,
                profile.name,
                profile.ts
            ]);
        }
        Ok(table.to_string())
    }
}

/// MeasurementBundleIdList just implements a newtype pattern
/// for a Vec<MeasurementBundleId> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementBundleIdList(Vec<MeasurementBundleId>);

impl ToTable for MeasurementBundleIdList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["bundle_id"]);
        for bundle_id in self.0.iter() {
            table.add_row(prettytable::row![bundle_id]);
        }
        Ok(table.to_string())
    }
}

/// MeasurementSystemProfileList just implements a newtype
/// pattern for a Vec<MeasurementSystemProfile> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementSystemProfileList(Vec<MeasurementSystemProfile>);

// When `profile show` gets called (for all entries), and the output format
// is the default table view, this gets used to print a pretty table.
impl ToTable for MeasurementSystemProfileList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row![
            "profile_id",
            "name",
            "created_ts",
            "attributes"
        ]);
        for profile in self.0.iter() {
            let mut attrs_table = prettytable::Table::new();
            attrs_table.add_row(prettytable::row!["name", "value"]);
            for attr_record in profile.attrs.iter() {
                attrs_table.add_row(prettytable::row![attr_record.key, attr_record.value]);
            }
            table.add_row(prettytable::row![
                profile.profile_id,
                profile.name,
                profile.ts,
                attrs_table
            ]);
        }
        Ok(table.to_string())
    }
}

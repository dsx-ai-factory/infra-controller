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
//! `measurement bundle` subcommand dispatcher + backing functions.

use std::str::FromStr;

use ::rpc::measured_boot::FromGrpcOpt;
use ::rpc::protos::measured_boot::{
    ListMeasurementBundleMachinesRequest, RenameMeasurementBundleRequest,
    ShowMeasurementBundleRequest, UpdateMeasurementBundleRequest,
};
use carbide_uuid::machine::MachineId;
use measured_boot::ToTable;
use measured_boot::bundle::MeasurementBundle;
use measured_boot::records::MeasurementBundleRecord;
use serde::Serialize;

use crate::attestation::measured_boot::MachineIdList;
use crate::attestation::measured_boot::bundle::args::{
    Create, Delete, FindClosestMatch, ListAll, ListMachines, Rename, ReportId, SetState, Show,
};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

impl Run for Create {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            create_for_id(&ctx.api_client, self).await?,
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

impl Run for SetState {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            set_state(&ctx.api_client, self).await?,
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
                show_all(&ctx.api_client, self).await?,
                &ctx.config.format,
                crate::Destination::Stdout(),
            )
        }
    }
}

impl Run for ReportId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        match find_closest_match(&ctx.api_client, FindClosestMatch::Report(self)).await? {
            Some(measurement_bundle) => crate::cli_output(
                measurement_bundle,
                &ctx.config.format,
                crate::Destination::Stdout(),
            ),
            None => {
                tracing::info!("No partially matching bundle found");
                Ok(())
            }
        }
    }
}

impl Run for ListAll {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListMachines {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_machines(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// create_for_id creates a new measurement bundle associated with the
/// profile w/ the provided profile ID.
async fn create_for_id(
    grpc_conn: &ApiClient,
    create: Create,
) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn.0.create_measurement_bundle(create).await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// delete deletes a measurement bundle with the provided ID.
async fn delete(grpc_conn: &ApiClient, delete: Delete) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn.0.delete_measurement_bundle(delete).await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// rename renames a measurement bundle with the provided name or ID.
async fn rename(grpc_conn: &ApiClient, rename: Rename) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn
        .0
        .rename_measurement_bundle(RenameMeasurementBundleRequest::try_from(rename)?)
        .await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// set_state updates the state of the bundle (e.g. active, obsolete, retired).
async fn set_state(
    grpc_conn: &ApiClient,
    set_state: SetState,
) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn
        .0
        .update_measurement_bundle(UpdateMeasurementBundleRequest::try_from(set_state)?)
        .await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// show_by_id dumps all info about a bundle for the given ID or name.
async fn show_by_id_or_name(
    grpc_conn: &ApiClient,
    show: Show,
) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn
        .0
        .show_measurement_bundle(ShowMeasurementBundleRequest::try_from(show)?)
        .await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// show_all dumps all info about all bundles.
async fn show_all(
    grpc_conn: &ApiClient,
    _get_by_id: Show,
) -> CarbideCliResult<MeasurementBundleList> {
    Ok(MeasurementBundleList(
        grpc_conn
            .0
            .show_measurement_bundles()
            .await?
            .bundles
            .into_iter()
            .map(|bundle| {
                MeasurementBundle::try_from(bundle)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementBundle>>>()?,
    ))
}

/// list lists all bundle ids.
async fn list(grpc_conn: &ApiClient) -> CarbideCliResult<MeasurementBundleRecordList> {
    Ok(MeasurementBundleRecordList(
        grpc_conn
            .0
            .list_measurement_bundles()
            .await?
            .bundles
            .into_iter()
            .map(|rec| {
                MeasurementBundleRecord::try_from(rec)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementBundleRecord>>>()?,
    ))
}

/// list_machines lists all machines associated with the provided
/// bundle ID or bundle name.
async fn list_machines(
    grpc_conn: &ApiClient,
    list_machines: ListMachines,
) -> CarbideCliResult<MachineIdList> {
    Ok(MachineIdList(
        grpc_conn
            .0
            .list_measurement_bundle_machines(ListMeasurementBundleMachinesRequest::try_from(
                list_machines,
            )?)
            .await?
            .machine_ids
            .iter()
            .map(|rec| {
                MachineId::from_str(rec)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MachineId>>>()?,
    ))
}

async fn find_closest_match(
    grpc_conn: &ApiClient,
    args: FindClosestMatch,
) -> CarbideCliResult<Option<MeasurementBundle>> {
    let response = grpc_conn.0.find_closest_bundle_match(args).await?;

    if response.bundle.is_none() {
        return Ok(None);
    }

    Ok(Some(
        MeasurementBundle::from_grpc_opt(response.bundle)
            .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))?,
    ))
}

/// MeasurementBundleRecordList just implements a newtype pattern
/// for a Vec<MeasurementBundleRecord> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementBundleRecordList(Vec<MeasurementBundleRecord>);

impl ToTable for MeasurementBundleRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row![
            Fg->"bundle_id",
            Fg->"profile_id",
            Fg->"name",
            Fg->"state",
            Fg->"created_ts"
        ]);
        for bundle in self.0.iter() {
            table.add_row(prettytable::row![
                bundle.bundle_id,
                bundle.profile_id,
                bundle.name,
                bundle.state,
                bundle.ts
            ]);
        }
        Ok(table.to_string())
    }
}

/// MeasurementBundleList just implements a newtype
/// pattern for a Vec<MeasurementBundle> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementBundleList(Vec<MeasurementBundle>);

// When `bundle show` gets called (for all entries), and the output format
// is the default table view, this gets used to print a pretty table.
impl ToTable for MeasurementBundleList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["bundle_id", "details", "values"]);
        for bundle in self.0.iter() {
            let mut details_table = prettytable::Table::new();
            details_table.add_row(prettytable::row!["profile_id", bundle.profile_id]);
            details_table.add_row(prettytable::row!["name", bundle.name]);
            details_table.add_row(prettytable::row!["state", bundle.state]);
            details_table.add_row(prettytable::row!["created_ts", bundle.ts]);
            let mut values_table = prettytable::Table::new();
            values_table.add_row(prettytable::row!["pcr_register", "value"]);
            for value_record in bundle.values.iter() {
                values_table.add_row(prettytable::row![
                    value_record.pcr_register,
                    value_record.sha_any
                ]);
            }
            table.add_row(prettytable::row![
                bundle.bundle_id,
                details_table,
                values_table
            ]);
        }
        Ok(table.to_string())
    }
}

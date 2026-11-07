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

use ::rpc::forge as forgerpc;
use prettytable::{Table, row};

use crate::errors::{CarbideCliError, CarbideCliResult};

/// Produces a table for printing a non-JSON representation of a
/// instance type to standard out.
///
/// * `itypes`  - A reference to an active DB transaction
/// * `verbose` - A bool to select more verbose output (e.g., include full rule details)
pub(super) fn convert_itypes_to_table(
    itypes: &[forgerpc::InstanceType],
    verbose: bool,
) -> CarbideCliResult<Box<Table>> {
    let mut table = Box::new(Table::new());
    let default_metadata = Default::default();

    if verbose {
        table.set_titles(row![
            "Id",
            "Name",
            "Description",
            "Version",
            "Created",
            "Labels",
            "Filters"
        ]);
    } else {
        table.set_titles(row![
            "Id",
            "Name",
            "Description",
            "Version",
            "Created",
            "Labels",
        ]);
    }

    for itype in itypes {
        let metadata = itype.metadata.as_ref().unwrap_or(&default_metadata);
        let labels = crate::metadata::fmt_labels_as_kv_pairs(Some(metadata));

        let default_attributes = forgerpc::InstanceTypeAttributes {
            desired_capabilities: vec![],
        };

        if verbose {
            table.add_row(row![
                itype.id,
                metadata.name,
                metadata.description,
                itype.version,
                itype.created_at(),
                labels.join(", "),
                serde_json::to_string_pretty(
                    &itype
                        .attributes
                        .as_ref()
                        .unwrap_or(&default_attributes)
                        .desired_capabilities
                )
                .map_err(CarbideCliError::JsonError)?,
            ]);
        } else {
            table.add_row(row![
                itype.id,
                metadata.name,
                metadata.description,
                itype.version,
                itype.created_at(),
                labels.join(", "),
            ]);
        }
    }

    Ok(table)
}

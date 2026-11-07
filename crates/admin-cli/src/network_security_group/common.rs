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

use ::rpc::forge::{self as forgerpc};
use prettytable::{Table, row};

use crate::errors::{CarbideCliError, CarbideCliResult};

/// Produces a table for printing a non-JSON representation of a
/// network security group to standard out.
///
/// * `nsgs`    - A reference to an active DB transaction
/// * `verbose` - A bool to select more verbose output (e.g., include full rule details)
pub(super) fn convert_nsgs_to_table(
    nsgs: &[forgerpc::NetworkSecurityGroup],
    verbose: bool,
) -> CarbideCliResult<Box<Table>> {
    let mut table = Box::new(Table::new());
    let default_metadata = Default::default();

    if verbose {
        table.set_titles(row![
            "Id",
            "Tenant Organization ID",
            "Name",
            "Description",
            "Version",
            "Created",
            "Created By",
            "Updated By",
            "Labels",
            "Stateful Egress",
            "Rules"
        ]);
    } else {
        table.set_titles(row![
            "Id",
            "Tenant Organization ID",
            "Name",
            "Description",
            "Version",
            "Created",
            "Created By",
            "Updated By",
            "Labels",
        ]);
    }

    for nsg in nsgs {
        let metadata = nsg.metadata.as_ref().unwrap_or(&default_metadata);
        let labels = crate::metadata::fmt_labels_as_kv_pairs(Some(metadata));

        let default_attributes = forgerpc::NetworkSecurityGroupAttributes {
            stateful_egress: false,
            rules: vec![],
        };

        if verbose {
            table.add_row(row![
                nsg.id,
                nsg.tenant_organization_id,
                metadata.name,
                metadata.description,
                nsg.version,
                nsg.created_at(),
                nsg.created_by(),
                nsg.updated_by(),
                labels.join(", "),
                nsg.attributes
                    .as_ref()
                    .unwrap_or(&default_attributes)
                    .stateful_egress,
                serde_json::to_string_pretty(
                    &nsg.attributes.as_ref().unwrap_or(&default_attributes).rules
                )
                .map_err(CarbideCliError::JsonError)?,
            ]);
        } else {
            table.add_row(row![
                nsg.id,
                nsg.tenant_organization_id,
                metadata.name,
                metadata.description,
                nsg.version,
                nsg.created_at(),
                nsg.created_by(),
                nsg.updated_by(),
                labels.join(", "),
            ]);
        }
    }

    Ok(table)
}

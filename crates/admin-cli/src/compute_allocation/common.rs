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

use crate::errors::CarbideCliResult;

/// Produces a table for printing a non-JSON representation of a
/// compute allocation to standard out.
pub(super) fn convert_compute_allocations_to_table(
    allocations: Vec<forgerpc::ComputeAllocation>,
    verbose: bool,
) -> CarbideCliResult<Box<Table>> {
    let mut table = Box::new(Table::new());
    let default_metadata = Default::default();

    if verbose {
        table.set_titles(row![
            "Id",
            "Tenant Organization ID",
            "Instance Type ID",
            "Count",
            "Name",
            "Description",
            "Version",
            "Created",
            "Created By",
            "Updated By",
            "Labels",
        ]);
    } else {
        table.set_titles(row![
            "Id",
            "Tenant Organization ID",
            "Instance Type ID",
            "Count",
            "Name",
            "Description",
            "Version",
            "Created",
        ]);
    }

    for allocation in allocations {
        let metadata = allocation.metadata.as_ref().unwrap_or(&default_metadata);
        let attributes = allocation.attributes.unwrap_or_default();
        let id = allocation
            .id
            .as_ref()
            .map(|compute_allocation_id| compute_allocation_id.to_string())
            .unwrap_or_default();

        let labels = metadata
            .labels
            .iter()
            .map(|label| {
                let key = &label.key;
                let value = label.value.as_deref().unwrap_or_default();
                format!("\"{key}:{value}\"")
            })
            .collect::<Vec<_>>();

        if verbose {
            table.add_row(row![
                id,
                allocation.tenant_organization_id,
                attributes.instance_type_id,
                attributes.count,
                metadata.name,
                metadata.description,
                allocation.version,
                allocation.created_at.unwrap_or_default(),
                allocation.created_by.unwrap_or_default(),
                allocation.updated_by.unwrap_or_default(),
                labels.join(", "),
            ]);
        } else {
            table.add_row(row![
                id,
                allocation.tenant_organization_id,
                attributes.instance_type_id,
                attributes.count,
                metadata.name,
                metadata.description,
                allocation.version,
                allocation.created_at.unwrap_or_default(),
            ]);
        }
    }

    Ok(table)
}

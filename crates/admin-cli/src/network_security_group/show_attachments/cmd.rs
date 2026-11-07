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

use std::collections::HashSet;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::{self as forgerpc};
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::network_security_group::common::convert_nsgs_to_table;
use crate::rpc::ApiClient;

/// Display details about objects that are using the
/// requested NSG, including propagation status of the
/// NSG across that object
pub(super) async fn show_attachments(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    // Grab the NSG details.
    let nsg = api_client
        .get_single_network_security_group(args.id.clone())
        .await?;

    // Grab the list of IDs for objects that are directly using this NSG.
    let nsg_attachments = api_client
        .get_network_security_group_attachments(args.id.clone())
        .await?;

    if nsg_attachments.vpc_ids.is_empty() && nsg_attachments.instance_ids.is_empty() {
        println!(
            "Network security group {} is not referenced by any objects",
            args.id.clone()
        );

        return Ok(());
    }

    // Next, prepare some sugar for users by grabbing the
    // propagation details for all objects using the NSG.
    let (vpcs, instances) = api_client
        .get_network_security_group_propagation_status(
            args.id.clone(),
            Some(nsg_attachments.vpc_ids.clone()),
            Some(nsg_attachments.instance_ids.clone()),
        )
        .await?;

    if is_json {
        // JSON output will get simple details.
        println!(
            "{{\"network_security_group\": {}, \"attachments\": {}, \"vpc_propagation_status\": {}, \"instance_propagation_status\": {}}}",
            serde_json::to_string_pretty(&nsg).map_err(CarbideCliError::JsonError)?,
            serde_json::to_string_pretty(&nsg_attachments).map_err(CarbideCliError::JsonError)?,
            serde_json::to_string_pretty(&vpcs).map_err(CarbideCliError::JsonError)?,
            serde_json::to_string_pretty(&instances).map_err(CarbideCliError::JsonError)?,
        );
    } else {
        let mut attachments_table = Box::new(Table::new());
        let mut propagation_table = Box::new(Table::new());

        attachments_table.set_titles(row!["Id", "Type"]);
        propagation_table.set_titles(row!["Id", "Type", "Relationship", "Propagated",]);

        for instance in nsg_attachments.instance_ids {
            attachments_table.add_row(row![instance, "INSTANCE",]);
        }

        for vpc in nsg_attachments.vpc_ids {
            attachments_table.add_row(row![vpc, "VPC",]);
        }

        for instance in instances {
            propagation_table.add_row(row![
                instance.id,
                "INSTANCE",
                "DIRECT",
                instance.status().as_str_name()
            ]);
        }

        for vpc in vpcs {
            propagation_table.add_row(row![vpc.id, "VPC", "DIRECT", vpc.status().as_str_name()]);

            let mut id_set = HashSet::<String>::new();

            // If the user wants to see an extended view
            // we can show them some details about objects
            // that are directly using the NSG _and_ objects
            // that are inheriting rules because a parent object
            // is a using the NSG.
            if args.include_indirect {
                for id in vpc.unpropagated_instance_ids {
                    id_set.insert(id);
                }

                for id in vpc.related_instance_ids {
                    // If it was seen already, then it's not propagated.
                    if id_set.contains(&id) {
                        propagation_table.add_row(row![
                            id,
                            "INSTANCE",
                            format!("INDIRECT via VPC {}", vpc.id),
                            forgerpc::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone
                                .as_str_name()
                        ]);
                    } else {
                        propagation_table.add_row(row![
                            id,
                            "INSTANCE",
                            format!("INDIRECT via VPC {}", vpc.id),
                            forgerpc::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull
                                .as_str_name()
                        ]);
                    }
                }
            }
        }

        convert_nsgs_to_table(&[nsg], false)?.printstd();
        println!("\nAttachments:");
        attachments_table.printstd();
        println!("\nPropagation:");
        propagation_table.printstd();
    }

    Ok(())
}

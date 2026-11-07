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

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// "Attaches" a network security group to an object (VPC/Instance)
/// by updating the config of the object.
pub(super) async fn attach(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    // Check that at least one of instance ID or VPC ID has been sent
    if args.instance_id.is_none() && args.vpc_id.is_none() {
        return Err(CarbideCliError::GenericError(
            "one of instance ID or VPC ID must be used".to_string(),
        ));
    }

    // Grab the instance for the ID if requested.
    if let Some(instance_id) = args.instance_id {
        let instance = api_client
            .get_one_instance(instance_id)
            .await?
            .instances
            .pop()
            .ok_or(CarbideCliError::UuidNotFound)?;

        // Grab the instance config for the target instance.
        // We'll modify the NSG ID field and then resubmit.
        let Some(mut config) = instance.config else {
            return Err(CarbideCliError::GenericError(
                "requested instance found without config".to_string(),
            ));
        };

        // Set the nsg ID
        config.network_security_group_id = Some(args.id.clone());

        // Resubmit the data back to the system.
        let _instance = api_client
            .update_instance_config(
                instance_id,
                instance.config_version,
                config,
                instance.metadata,
            )
            .await?;

        println!(
            "Network security group {} successfully attached to instance {}",
            args.id.clone(),
            instance_id
        );
    }

    // Grab the VPC for the ID if requested.
    if let Some(v) = args.vpc_id {
        let vpc = api_client
            .0
            .find_vpcs_by_ids(&[v])
            .await?
            .vpcs
            .pop()
            .ok_or(CarbideCliError::UuidNotFound)?;

        // Submit the VPC details back to the system but change the
        // NSG ID value.
        let _vpc = api_client
            .update_vpc_config(v, vpc.version, vpc.metadata, Some(args.id.clone()))
            .await?;

        println!(
            "Network security group {} successfully attached to VPC {}",
            args.id.clone(),
            v
        );
    }

    Ok(())
}

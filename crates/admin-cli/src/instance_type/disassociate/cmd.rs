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

use rpc::TenantState;
use rpc::forge::RemoveMachineInstanceTypeAssociationRequest;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn remove_association(
    args: Args,
    cloud_unsafe_operation_allowed: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let instance = api_client
        .0
        .find_instance_by_machine_id(args.machine_id)
        .await?;

    if let Some(instance) = instance.instances.first() {
        if let Some(status) = &instance.status
            && let Some(tenant) = &status.tenant
        {
            match tenant.state() {
                TenantState::Terminating | TenantState::Terminated => {
                    if !cloud_unsafe_operation_allowed {
                        return Err(CarbideCliError::GenericError(
                                r#"A instance is already allocated to this machine, but terminating.
        Removing instance type will create a mismatch between cloud and carbide. If you are sure, run this command again with --cloud-unsafe-op=<username> flag before `instance-type`."#.to_string(),
        ));
                    }
                    remove_association_api(api_client, &args).await?;
                    return Ok(());
                }
                _ => {}
            }
        }
        return Err(CarbideCliError::GenericError(
            "A instance is already allocated to this machine. You can remove an instance-type association only in Teminating state.".to_string(),
        ));
    } else {
        remove_association_api(api_client, &args).await?;
    }

    Ok(())
}

async fn remove_association_api(
    api_client: &ApiClient,
    args: &Args,
) -> Result<(), CarbideCliError> {
    let req: RemoveMachineInstanceTypeAssociationRequest = args.into();
    api_client
        .0
        .remove_machine_instance_type_association(req)
        .await?;
    println!("Association is removed successfully!!");
    Ok(())
}

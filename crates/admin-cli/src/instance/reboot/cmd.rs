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

use ::rpc::forge::{self as forgerpc, InstancePowerRequest};
use carbide_uuid::instance::InstanceId;
use eyre::WrapErr;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

fn with_reboot_context<T>(
    result: Result<T, tonic::Status>,
    instance_id: InstanceId,
) -> CarbideCliResult<T> {
    result
        .wrap_err_with(|| format!("failed to request reboot for instance {instance_id}"))
        .map_err(CarbideCliError::from)
}

pub(super) async fn handle_reboot(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    with_reboot_context(
        api_client
            .0
            .invoke_instance_power(InstancePowerRequest {
                instance_id: Some(args.instance),
                operation: forgerpc::instance_power_request::Operation::PowerReset as i32,
                boot_with_custom_ipxe: args.custom_pxe,
                apply_updates_on_reboot: args.apply_updates_on_reboot,
            })
            .await,
        args.instance,
    )?;
    println!(
        "Reboot for instance {} is requested successfully!",
        args.instance
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::*;

    #[test]
    fn reboot_error_includes_instance_context_and_preserves_source() {
        let instance_id = "12345678-1234-5678-90ab-cdef01234567"
            .parse::<InstanceId>()
            .unwrap();

        let error = with_reboot_context::<()>(
            Err(tonic::Status::unavailable("API unavailable")),
            instance_id,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("failed to request reboot for instance {instance_id}")
        );
        let CarbideCliError::EyreReport(report) = error else {
            panic!("expected an EyreReport");
        };
        let status = report
            .downcast_ref::<tonic::Status>()
            .expect("tonic status should remain in the error chain");
        assert_eq!(status.code(), Code::Unavailable);
        assert_eq!(status.message(), "API unavailable");
    }
}

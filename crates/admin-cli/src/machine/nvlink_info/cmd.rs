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

use super::args::{NvlinkInfoArgs, NvlinkInfoPopulateArgs};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[allow(deprecated)]
pub(super) async fn handle_nvlink_info_show(
    args: NvlinkInfoArgs,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine = api_client.get_machine(args.machine_id).await?;

    // Check if this is an MNNVL machine (GB200)
    let is_mnnvl = machine
        .discovery_info
        .as_ref()
        .and_then(|info| info.dmi_data.as_ref())
        .map(|dmi| dmi.product_name.contains("GB200"))
        .unwrap_or(false);

    if !is_mnnvl {
        return Err(CarbideCliError::GenericError(format!(
            "Machine {} is not an MNNVL machine",
            args.machine_id
        )));
    }

    match machine.nvlink_info {
        Some(nvlink_info) => {
            println!("{}", serde_json::to_string_pretty(&nvlink_info)?);
        }
        None => {
            return Err(CarbideCliError::GenericError(format!(
                "Machine {} has no nvlink_info in database",
                args.machine_id
            )));
        }
    }

    Ok(())
}

/// Rendered when the retained `populate` compatibility command is invoked.
pub(super) const POPULATE_UNSUPPORTED_MESSAGE: &str = "`machine nvlink-info populate` is deprecated; \
the NICo NVLink partition manager populates and repairs machine NVLink info automatically. \
Use `machine nvlink-info show` to inspect it";

/// Compatibility stub: performs no side effects and always returns `UnsupportedOperation`.
pub(super) fn handle_nvlink_info_populate(_args: NvlinkInfoPopulateArgs) -> CarbideCliResult<()> {
    Err(CarbideCliError::UnsupportedOperation(
        POPULATE_UNSUPPORTED_MESSAGE,
    ))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn populate_returns_unsupported_without_side_effects() {
        let args = NvlinkInfoPopulateArgs::try_parse_from([
            "populate",
            "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg",
            "--update-db",
        ])
        .expect("legacy arguments should still parse");
        let error = handle_nvlink_info_populate(args)
            .expect_err("the compatibility command must not populate nvlink info");

        assert_eq!(
            error.to_string(),
            format!("unsupported operation: {POPULATE_UNSUPPORTED_MESSAGE}")
        );
    }
}

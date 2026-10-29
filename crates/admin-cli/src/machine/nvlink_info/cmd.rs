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

use model::hardware_info::HardwareInfo;

use super::args::{NvlinkInfoArgs, NvlinkInfoPopulateArgs};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn handle_nvlink_info_show(
    args: NvlinkInfoArgs,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine = api_client.get_machine(args.machine_id).await?;

    if !is_mnnvl_machine(&machine)? {
        return Err(CarbideCliError::GenericError(format!(
            "Machine {} is not an MNNVL machine",
            args.machine_id
        )));
    }

    match machine.status.and_then(|status| status.nvlink_info) {
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

/// Applies the same MNNVL capability check NICo uses before it stores NVLink info.
fn is_mnnvl_machine(machine: &::rpc::Machine) -> CarbideCliResult<bool> {
    let Some(discovery_info) = machine
        .status
        .as_ref()
        .and_then(|status| status.discovery_info.clone())
    else {
        return Ok(false);
    };
    Ok(HardwareInfo::try_from(discovery_info)?.is_mnnvl_capable())
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
    use ::rpc::forge::MachineStatus;
    use ::rpc::machine_discovery::{DiscoveryInfo, DmiData, Gpu, GpuPlatformInfo};
    use carbide_test_support::value_scenarios;
    use clap::Parser;

    use super::*;

    fn machine_with_gpu(gpu_name: &str, dmi_product_name: &str) -> ::rpc::Machine {
        ::rpc::Machine {
            status: Some(MachineStatus {
                discovery_info: Some(DiscoveryInfo {
                    dmi_data: Some(DmiData {
                        product_name: dmi_product_name.to_string(),
                        ..Default::default()
                    }),
                    gpus: vec![Gpu {
                        name: gpu_name.to_string(),
                        platform_info: Some(GpuPlatformInfo::default()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn show_accepts_every_mnnvl_platform() {
        value_scenarios!(
            run = |(gpu_name, dmi_product_name): (&str, &str)| {
                is_mnnvl_machine(&machine_with_gpu(gpu_name, dmi_product_name)).unwrap()
            };
            "MNNVL GPU name outside GB200" {
                ("NVIDIA GB300", "P4242") => true,
            }
            "MNNVL marker only in DMI product name" {
                ("NVIDIA Graphics Device", "VR NVL144") => true,
            }
            "non-MNNVL GPU" {
                ("NVIDIA H100 80GB HBM3", "DGXH100") => false,
            }
        );
    }

    #[test]
    fn show_rejects_machine_without_discovery_info() {
        assert!(!is_mnnvl_machine(&::rpc::Machine::default()).unwrap());
    }

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

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

use ::rpc::admin_cli::OutputFormat;
use ::rpc::{Machine, forge as forgerpc};
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::MachineId;
use model::hardware_info::gpu_name_indicates_mnnvl;

use super::args::{NvlinkInfoArgs, NvlinkInfoPopulateArgs};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Returns an error unless the machine's discovery info identifies MNNVL hardware (GB200|GB300|VR NVL)
#[allow(deprecated)]
fn ensure_mnnvl_machine(machine: &Machine, machine_id: MachineId) -> CarbideCliResult<()> {
    let is_mnnvl = machine.discovery_info.as_ref().is_some_and(|info| {
        info.dmi_data
            .as_ref()
            .is_some_and(|dmi| gpu_name_indicates_mnnvl(&dmi.product_name))
            || info
                .gpus
                .iter()
                .any(|gpu| gpu_name_indicates_mnnvl(&gpu.name))
    });

    if !is_mnnvl {
        return Err(CarbideCliError::GenericError(format!(
            "Machine {} is not an MNNVL machine",
            machine_id
        )));
    }

    Ok(())
}

#[allow(deprecated)]
pub(super) async fn handle_nvlink_info_show(
    args: NvlinkInfoArgs,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine = api_client.get_machine(args.machine_id).await?;

    ensure_mnnvl_machine(&machine, args.machine_id)?;

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

#[allow(deprecated)]
pub(super) async fn handle_nvlink_info_populate(
    args: NvlinkInfoPopulateArgs,
    _output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine = api_client.get_machine(args.machine_id).await?;
    let update_db = args.update_db;

    ensure_mnnvl_machine(&machine, args.machine_id)?;

    let bmc_ip = machine
        .bmc_info
        .as_ref()
        .and_then(|b| b.ip.clone())
        .ok_or_else(|| CarbideCliError::GenericError("No BMC IP available".to_string()))?;

    // Fetch Redfish data first (serial + tray are required to resolve the NMX-C endpoint mapping).
    let uri = format!("https://{}/redfish/v1/Chassis/CBC_0", bmc_ip);

    let redfish_response = api_client
        .0
        .redfish_browse(uri.clone())
        .await
        .map_err(|e| CarbideCliError::GenericError(format!("Redfish call failed: {}", e)))?;

    let json: serde_json::Value = serde_json::from_str(&redfish_response.text).map_err(|e| {
        CarbideCliError::GenericError(format!("Failed to parse Redfish response: {}", e))
    })?;

    // Extract Oem.Nvidia.ComputeTrayIndex
    let tray_index = json
        .get("Oem")
        .and_then(|oem| oem.get("Nvidia"))
        .and_then(|nvidia| nvidia.get("ComputeTrayIndex"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .ok_or_else(|| {
            CarbideCliError::GenericError("No tray_index found in Redfish response".to_string())
        })?;

    // Extract SerialNumber
    let serial_number = json
        .get("SerialNumber")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            CarbideCliError::GenericError("No SerialNumber found in Redfish response".to_string())
        })?;

    // Get domain UUID and GPUs by tray index
    let gpu_list_response = api_client
        .0
        .nmxc_browse(forgerpc::NmxcBrowseRequest {
            chassis_serial: serial_number.clone(),
            rack_id: None,
            operation: forgerpc::NmxcBrowseOperation::GpuInfoList as i32,
            gpu_uid: 0,
        })
        .await
        .map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to fetch NMX-C GPU list: {}", e))
        })?;

    if gpu_list_response.code < 200 || gpu_list_response.code >= 300 {
        return Err(CarbideCliError::GenericError(format!(
            "NMX-C GPU list request failed with HTTP {}: {}",
            gpu_list_response.code, gpu_list_response.body
        )));
    }

    let list_json: serde_json::Value =
        serde_json::from_str(&gpu_list_response.body).map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to parse NMX-C GPU list response: {}", e))
        })?;

    let domain_uuid = list_json
        .get("server_header")
        .and_then(|h| h.get("domain_uuid"))
        .and_then(|v| v.as_str())
        .none_if_empty()
        .ok_or_else(|| {
            CarbideCliError::GenericError("No domain_uuid in NMX-C server_header".to_string())
        })?
        .parse::<uuid::Uuid>()
        .map_err(|e| {
            CarbideCliError::GenericError(format!("Failed to parse domain_uuid: {}", e))
        })?;

    let gpus_json = list_json
        .get("gpu_info_list")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            CarbideCliError::GenericError("No Gpus array in NMX-C GPU list response".to_string())
        })?;

    let mut gpus: Vec<forgerpc::NvLinkGpu> = Vec::new();
    for gpu_json in gpus_json {
        let gpu_tray_index = gpu_json
            .get("loc")
            .and_then(|loc| loc.get("tray_index"))
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .ok_or_else(|| {
                CarbideCliError::GenericError(
                    "GPU entry missing loc.tray_index in NMX-C GPU list response".to_string(),
                )
            })?;
        if gpu_tray_index != tray_index {
            continue;
        }

        let gpu_device_id = gpu_json
            .get("gpu_id")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .ok_or_else(|| {
                CarbideCliError::GenericError(
                    "GPU entry missing gpu_id in NMX-C GPU list response".to_string(),
                )
            })?;
        let gpu_device_uid = gpu_json
            .get("gpu_uid")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                CarbideCliError::GenericError(
                    "GPU entry missing gpu_uid in NMX-C GPU list response".to_string(),
                )
            })?;

        let gpu_slot_id = gpu_json
            .get("loc")
            .and_then(|loc| {
                loc.get("slot_id").and_then(|v| v.as_i64()).or_else(|| {
                    loc.get("location")
                        .and_then(|location| location.get("slot_id"))
                        .and_then(|v| v.as_i64())
                })
            })
            .map(|v| v as i32)
            .ok_or_else(|| {
                CarbideCliError::GenericError(
                    "GPU entry missing loc.slot_id or loc.location.slot_id in NMX-C GPU list response"
                        .to_string(),
                )
            })?;

        gpus.push(forgerpc::NvLinkGpu {
            device_id: gpu_device_id,
            guid: gpu_device_uid,
            tray_index: gpu_tray_index,
            slot_id: gpu_slot_id,
        });
    }

    if gpus.is_empty() {
        return Err(CarbideCliError::GenericError(format!(
            "No GPUs in NMX-C GPU list with tray_index={} (chassis serial {})",
            tray_index, serial_number
        )));
    }

    // Build the nvlink_info structure for RPC
    let nvlink_info_rpc = forgerpc::MachineNvLinkInfo {
        domain_uuid: Some(carbide_uuid::nvlink::NvLinkDomainId::from(domain_uuid)),
        gpus: gpus.clone(),
        chassis_serial: serial_number.clone(),
    };

    // Build the nvlink_info structure as JSON for display
    let nvlink_info = serde_json::json!({
        "domain_uuid": domain_uuid,
        "chassis_serial": serial_number,
        "gpus": gpus.iter().map(|g| serde_json::json!({
            "device_id": g.device_id,
            "guid": g.guid,
            "tray_index": g.tray_index,
            "slot_id": g.slot_id,
        })).collect::<Vec<_>>(),
    });

    if update_db {
        api_client
            .update_machine_nvlink_info(args.machine_id, nvlink_info_rpc)
            .await?;
        println!("Updated nvlink_info in db with the following nvlink-info:");
    } else {
        println!("\n\n Use --update-db option to apply the following nvlink-info:");
    }

    println!("{}", serde_json::to_string_pretty(&nvlink_info)?);

    Ok(())
}

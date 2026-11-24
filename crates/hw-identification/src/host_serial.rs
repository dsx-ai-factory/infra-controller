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

//! Serial number sources used to identify compute trays in host inventory.

use crate::hw::{nvidia_openbmc, nvidia_titania_bianca_pdb, nvidia_vr_nvl_pdb};

/// Chassis whose Assembly members are used by the built-in host serial rule.
pub const HOST_SERIAL_ASSEMBLY_CHASSIS_IDS: &[&str] = &[nvidia_openbmc::COMPUTE_TRAY_CHASSIS_ID];

/// Identifies the assembly used for the GB200 or Vera Rubin compute tray's inventory serial.
pub fn is_compute_tray_assembly(chassis_id: &str, assembly_model: Option<&str>) -> bool {
    HOST_SERIAL_ASSEMBLY_CHASSIS_IDS.contains(&chassis_id)
        && matches!(
            assembly_model,
            Some(
                nvidia_openbmc::GB200_PDB_CHASSIS_ASSEMBLY_MODEL
                    | nvidia_titania_bianca_pdb::FRU_PRODUCT_NAME
                    | nvidia_vr_nvl_pdb::CHASSIS_FRU_ASSEMBLY_MODEL
            )
        )
}

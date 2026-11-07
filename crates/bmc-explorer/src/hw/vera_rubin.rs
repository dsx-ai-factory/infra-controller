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

use crate::hw::BiosAttr;

// Vera Rubin compute-tray host BMC: Systems/System_0, ServiceRoot Product "VR NVL72".
// Shares TPM / EmbeddedUefiShell with GB200; uses GpuExposeAsPcie instead of GB200's
// Socket{0,1}Pcie6DisableOptionROM knobs.

/// `Model` value reported for the Power Distribution Board (PDB) chassis FRU under
/// `/redfish/v1/Chassis/Chassis_0/Assembly` on a Vera Rubin compute tray. `P4107` is
/// NVIDIA's board/PCA design code (the same FRU carries `Name` "PDB_0 Chassis FRU
/// Assembly0" and `PartNumber` "675-24109-...."). `Model` is the stable, machine-readable
/// key we match on; its `SerialNumber` is the compute-tray serial the inventory
/// (Nautobot / expected-machine) tracks -- the raw `Chassis_0.SerialNumber` is a
/// different, internal board serial that must NOT be used for matching.
pub const VERA_RUBIN_PDB_CHASSIS_FRU_ASSEMBLY_MODEL: &str = "P4107";

/// Assembly `Model` on `Chassis_0` whose `SerialNumber` is the source of truth for
/// Vera Rubin expected-machine matching.
pub fn chassis_assembly_serial_model(model: &str) -> bool {
    matches!(model, VERA_RUBIN_PDB_CHASSIS_FRU_ASSEMBLY_MODEL)
}

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 3] = [
    BiosAttr::new_str("TPM", "Enabled"),
    BiosAttr::new_str("EmbeddedUefiShell", "Disabled"),
    BiosAttr::new_bool("GpuExposeAsPcie", true),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chassis_assembly_serial_model_matches_vera_rubin() {
        assert!(chassis_assembly_serial_model(
            VERA_RUBIN_PDB_CHASSIS_FRU_ASSEMBLY_MODEL
        ));
        assert!(!chassis_assembly_serial_model("GB200 NVL"));
        assert!(!chassis_assembly_serial_model("1331226010330"));
        assert!(!chassis_assembly_serial_model("2101326000053"));
    }
}

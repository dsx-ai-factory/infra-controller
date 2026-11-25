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

//! Built-in rules for selecting the host inventory serial number.

use std::borrow::Cow::Borrowed;

use super::{HostSerialRule, HostSerialSource};
use crate::hw::{nvidia_openbmc, nvidia_titania_bianca_pdb, nvidia_vr_nvl_pdb};
use crate::pattern::{Pattern, exact};

const COMPUTE_TRAY_CHASSIS: Pattern = exact(nvidia_openbmc::COMPUTE_TRAY_CHASSIS_ID);

/// Built-in host serial rules in match order.
pub(super) static DEFAULT_HOST_SERIAL_RULES: &[HostSerialRule] = &[
    // The GB200 Entity Manager configuration publishes CHASSIS_SERIAL_NUMBER on this assembly.
    // https://github.com/NVIDIA/openbmc/blob/4f37649dda3d7ad2ec0e4ab65e671f30d52974c3/meta-nvidia/meta-prime/meta-graceblackwell/meta-gb200nvl/meta-bmc/recipes-phosphor/configuration/entity-manager/files/Chassis_1RU.json#L1737-L1747
    HostSerialRule {
        id: Borrowed("gb200-pdb-chassis-fru"),
        hardware_class: None,
        source: HostSerialSource::ChassisAssembly {
            chassis_id: COMPUTE_TRAY_CHASSIS,
            name: Some(exact(nvidia_openbmc::PDB_CHASSIS_FRU_NAME)),
            model: Some(exact(nvidia_openbmc::GB200_PDB_CHASSIS_ASSEMBLY_MODEL)),
            member_id: None,
        },
    },
    // The Redfish report in #9059 gives both chassis and product FRUs the Titania-Bianca model.
    // Entity Manager maps CHASSIS_SERIAL_NUMBER to the chassis FRU and PRODUCT_SERIAL_NUMBER to the product FRU.
    // https://github.com/dsx-ai-factory/infra-controller/issues/9059
    // https://github.com/NVIDIA/openbmc/blob/af794c84585b94c86b917b2e04ad64845eadfb6f/meta-nvidia/meta-prime/meta-graceblackwell/meta-gb200nvl/meta-bmc/recipes-phosphor/configuration/entity-manager/files/Chassis_1RU.json#L1742-L1795
    HostSerialRule {
        id: Borrowed("titania-bianca-pdb-chassis-fru"),
        hardware_class: None,
        source: HostSerialSource::ChassisAssembly {
            chassis_id: COMPUTE_TRAY_CHASSIS,
            name: Some(exact(nvidia_openbmc::PDB_CHASSIS_FRU_NAME)),
            model: Some(exact(nvidia_titania_bianca_pdb::FRU_PRODUCT_NAME)),
            member_id: None,
        },
    },
    // The original Vera Rubin inventory fix identifies this P4107 chassis FRU as the source of the tray serial.
    // https://github.com/dsx-ai-factory/infra-controller/blob/a752ec4a4004c4dce055f8e934d9f9438974da7e/crates/bmc-explorer/src/hw/vera_rubin.rs#L24-L31
    HostSerialRule {
        id: Borrowed("vr-nvl-pdb-chassis-fru"),
        hardware_class: None,
        source: HostSerialSource::ChassisAssembly {
            chassis_id: COMPUTE_TRAY_CHASSIS,
            name: Some(exact(nvidia_vr_nvl_pdb::CHASSIS_FRU_NAME)),
            model: Some(exact(nvidia_vr_nvl_pdb::CHASSIS_FRU_ASSEMBLY_MODEL)),
            member_id: None,
        },
    },
    // VRB-2601-28.00 reports the P4107 chassis FRU under the unindexed PDB name.
    HostSerialRule {
        id: Borrowed("vr-nvl-pdb-chassis-fru-unindexed"),
        hardware_class: None,
        source: HostSerialSource::ChassisAssembly {
            chassis_id: COMPUTE_TRAY_CHASSIS,
            name: Some(exact(nvidia_openbmc::PDB_CHASSIS_FRU_NAME)),
            model: Some(exact(nvidia_vr_nvl_pdb::CHASSIS_FRU_ASSEMBLY_MODEL)),
            member_id: None,
        },
    },
];

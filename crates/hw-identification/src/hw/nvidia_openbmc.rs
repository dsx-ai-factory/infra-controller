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

//! Inventory names assigned by NVIDIA OpenBMC platform configurations.

/// Name of the [GB200 compute-tray chassis], also used for the [Vera Rubin PDB assembly].
///
/// [GB200 compute-tray chassis]: https://github.com/NVIDIA/openbmc/blob/4f37649dda3d7ad2ec0e4ab65e671f30d52974c3/meta-nvidia/meta-prime/meta-graceblackwell/meta-gb200nvl/meta-bmc/recipes-phosphor/configuration/entity-manager/files/Chassis_1RU.json#L1679
/// [Vera Rubin PDB assembly]: https://github.com/dsx-ai-factory/infra-controller/blob/eca06f3f26d7d7d6a36808914365bac8dba0051f/crates/bmc-explorer/src/hw/vera_rubin.rs#L24-L31
pub const COMPUTE_TRAY_CHASSIS_ID: &str = "Chassis_0";

/// Fixed `Model` on `PDB Chassis FRU Assembly0` in the [GB200 Entity Manager configuration].
///
/// [GB200 Entity Manager configuration]: https://github.com/NVIDIA/openbmc/blob/4f37649dda3d7ad2ec0e4ab65e671f30d52974c3/meta-nvidia/meta-prime/meta-graceblackwell/meta-gb200nvl/meta-bmc/recipes-phosphor/configuration/entity-manager/files/Chassis_1RU.json#L1737-L1747
pub const GB200_PDB_CHASSIS_ASSEMBLY_MODEL: &str = "GB200 NVL";

/// Chassis FRU assembly Name in the same GB200 Entity Manager configuration.
pub const PDB_CHASSIS_FRU_NAME: &str = "PDB Chassis FRU Assembly0";

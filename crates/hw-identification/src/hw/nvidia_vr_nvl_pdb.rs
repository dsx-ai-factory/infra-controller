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

//! NVIDIA Vera Rubin NVL compute tray power distribution board (PDB).

/// Board design code reported as `Model` on the [Vera Rubin PDB chassis FRU assembly].
///
/// [Vera Rubin PDB chassis FRU assembly]: https://github.com/dsx-ai-factory/infra-controller/blob/eca06f3f26d7d7d6a36808914365bac8dba0051f/crates/bmc-explorer/src/hw/vera_rubin.rs#L24-L31
pub const CHASSIS_FRU_ASSEMBLY_MODEL: &str = "P4107";

/// Name of the Vera Rubin PDB chassis FRU associated with [`CHASSIS_FRU_ASSEMBLY_MODEL`].
pub const CHASSIS_FRU_NAME: &str = "PDB_0 Chassis FRU Assembly0";

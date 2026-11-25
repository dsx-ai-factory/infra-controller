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

//! NVIDIA Titania-Bianca compute tray power distribution board (PDB).

/// PDB FRU product name observed in the [Redfish capture for issue #9059].
/// The [GB200 Entity Manager configuration] reads `$PRODUCT_PRODUCT_NAME` for the
/// `Model` of both `PDB Chassis FRU Assembly0` and `PDB Product FRU Assembly2`.
///
/// [GB200 Entity Manager configuration]: https://github.com/NVIDIA/openbmc/blob/af794c84585b94c86b917b2e04ad64845eadfb6f/meta-nvidia/meta-prime/meta-graceblackwell/meta-gb200nvl/meta-bmc/recipes-phosphor/configuration/entity-manager/files/Chassis_1RU.json#L1742-L1752
/// [Redfish capture for issue #9059]: https://github.com/dsx-ai-factory/infra-controller/issues/9059
pub const FRU_PRODUCT_NAME: &str = "Titania-Bianca Compute Tray PDB";

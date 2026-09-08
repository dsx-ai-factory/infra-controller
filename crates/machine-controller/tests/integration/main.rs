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

// Keep the suites in one executable: sqlx-testing's migrated template is process-local.
mod bmc_rotation;
mod dpu_uefi_rotation;
mod env;
mod firmware_upgrade_completion;
mod gb200_host_interface;
mod host_uefi_rotation;
mod host_uefi_setup;
mod maintenance;
mod power_management;
mod rack_firmware_upgrade;

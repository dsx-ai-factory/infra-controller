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

use serde::{Deserialize, Serialize};

// FirmwareFlashReport captures the outcome of each step in the
// firmware flash lifecycle. Built by scout after executing the
// ApplyFirmware operation and sent back to the API as part of an
// MlxObservation.
//
// Each optional step (reset, verify_image, verify_version) is
// controlled by the corresponding flag in FlashOptions.
// A None value means the step was not requested; Some(true/false)
// means it was attempted and the result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirmwareFlashReport {
    // Whether the firmware was successfully flashed via flint.
    pub flashed: bool,
    // Whether the device was successfully reset via mlxfwreset.
    // None if config.reset was false (not requested).
    pub reset: Option<bool>,
    // Whether the firmware image on the device was verified against
    // the source image via flint verify.
    // None if config.verify_image was false (not requested).
    pub verified_image: Option<bool>,
    // Whether the firmware version on the device matches the expected
    // version. None if config.verify_version was false (not requested).
    pub verified_version: Option<bool>,
    // The firmware version observed after flashing. Live runs query it through
    // `mlxfwmanager`; dry runs use the configured target to describe the
    // simulated lifecycle. None means a live query failed, returned no current
    // version, or the step was not requested.
    pub observed_version: Option<String>,
    // The expected firmware version from the config, if one was set.
    pub expected_version: Option<String>,
}

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

use std::path::PathBuf;
use std::time::Duration;

use carbide_firmware::FirmwareConfig;

#[derive(Clone)]
pub struct PreingestionManagerConfig {
    pub run_interval: Duration,
    pub concurrency_limit: usize,
    pub hgx_bmc_gpu_reboot_delay: Duration,
    pub max_concurrent_bfb_copies: usize,
    pub autoupdate: bool,
    pub no_reset_retries: bool,
    pub firmware_download_cache_directory: PathBuf,
    pub firmware: FirmwareConfig,
}

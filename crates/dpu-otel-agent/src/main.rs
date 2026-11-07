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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use std::time::Duration;

fn main() -> eyre::Result<()> {
    carbide_host_support::init_logging("forge-dpu-otel-agent")?;
    // We need a multi-threaded runtime since background threads will queue work
    // on it, and the foreground thread might not be blocked onto the runtime
    // at all points in time
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(otel_agent::start(otel_agent::Options::load()))?;
    rt.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}

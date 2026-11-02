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

use carbide_test_harness::TestHarness;
use carbide_uuid::power_shelf::PowerShelfId;

/// Creates a power shelf through api-db test support for API integration tests.
pub(crate) async fn create_custom_power_shelf(
    env: &TestHarness,
    name: &str,
    capacity: Option<u32>,
    voltage: Option<u32>,
) -> Result<PowerShelfId, Box<dyn std::error::Error>> {
    let mut txn = env.db_txn().await;
    let power_shelf = db::test_support::power_shelf::create_random_with_config(
        &mut txn,
        name,
        capacity.or(Some(100)),
        voltage.or(Some(240)),
    )
    .await?;
    txn.commit().await?;

    Ok(power_shelf.id)
}

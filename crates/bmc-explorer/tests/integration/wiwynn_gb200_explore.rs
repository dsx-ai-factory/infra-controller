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
use bmc_explorer::nv_generate_exploration_report;
use bmc_mock::test_support;
use tokio::test;

use crate::common;

#[test]
async fn chassis_position_remains_the_source_for_gb200() {
    let h = test_support::wiwynn_gb200_bmc_at_rack_position(11).await;
    let mut report =
        nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &common::explorer_config())
            .await
            .unwrap();

    let cbc = report
        .chassis
        .iter()
        .find(|chassis| chassis.id.starts_with("CBC_"))
        .expect("GB200 compute tray must expose a CBC chassis");
    assert_eq!(cbc.physical_slot_number, Some(10));
    assert_eq!(cbc.compute_tray_index, Some(0));

    report.parse_position_info();
    assert_eq!(report.physical_slot_number, Some(10));
    assert_eq!(report.compute_tray_index, Some(0));
}

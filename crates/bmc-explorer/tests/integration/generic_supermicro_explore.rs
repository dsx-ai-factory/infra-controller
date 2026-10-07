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
use bmc_explorer::hw::HwType;
use bmc_explorer::test_support::detect_hw_type;
use bmc_mock::test_support;
use tokio::test;

use crate::common;

/// Regression guard for the GB300 detection decouple: a Supermicro-vendor BMC with
/// **no** NVIDIA GB300 GPU chassis (e.g. an SMC ancillary node) must stay generic
/// `Supermicro`, not `SupermicroGb300`. The SMC GB300 arm is gated on `is_gb300()`,
/// so a box without the `NVIDIA GB300` GPU chassis must not reach it.
#[test]
async fn generic_supermicro_is_not_gb300() {
    let h = test_support::generic_supermicro_bmc().await;
    assert_eq!(
        detect_hw_type(h.service_root, &common::explorer_config())
            .await
            .unwrap(),
        Some(HwType::Supermicro),
        "a non-GB300 Supermicro must not be detected as SupermicroGb300",
    );
}

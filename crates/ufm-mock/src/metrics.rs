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

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};

#[derive(Clone, Debug)]
pub(crate) struct Metrics {
    reconciliation: Counter<u64>,
}

impl Metrics {
    pub(crate) fn new(meter: &Meter) -> Self {
        Self {
            reconciliation: meter
                .u64_counter("carbide_ufm_mock_inventory_reconciliations_total")
                .with_description("Inventory reconciliation attempts by outcome.")
                .build(),
        }
    }

    pub(crate) fn record_reconciliation(&self, outcome: &'static str) {
        self.reconciliation
            .add(1, &[KeyValue::new("outcome", outcome)]);
    }
}

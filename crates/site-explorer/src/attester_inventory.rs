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
use db::DatabaseError;
use model::site_explorer::EndpointExplorationReport;
use sqlx::PgConnection;

/// A class growing a second attester set means hardware under one policy
/// stopped matching its peers, which is worth alerting on. The first set for a
/// brand-new class also emits, which is how a site sees hardware arrive.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "attestation_attester_set_new",
    metric_name = "carbide_attestation_attester_sets_total",
    component = "site-explorer",
    log = warn,
    metric = counter,
    message = "new attester set recorded for a hardware class",
    describe = "Number of previously unseen SPDM-capable attester sets recorded for a hardware \
                class"
)]
struct AttestationAttesterSetNew {
    /// Unbounded, as are the digests, so neither can be a label.
    #[context]
    hardware_class: String,
    #[context]
    attester_digest: String,
    #[context]
    attester_ids: String,
}

/// Records the attesters an exploration saw against the class it derived, in
/// the caller's transaction, so the pair is stored with the report it came
/// from.
///
/// A report with no class or no `ComponentIntegrity` collection records
/// nothing: without both halves there is no observation to attribute.
pub(crate) async fn record(
    report: &EndpointExplorationReport,
    txn: &mut PgConnection,
) -> Result<(), DatabaseError> {
    let (Some(hardware_class), Some(attesters)) =
        (report.hardware_class.as_deref(), report.attester_set())
    else {
        return Ok(());
    };

    if db::hardware_class_attesters::record(txn, hardware_class, &attesters).await? {
        carbide_instrument::emit(AttestationAttesterSetNew {
            hardware_class: hardware_class.to_string(),
            attester_digest: attesters.digest,
            attester_ids: attesters.ids.join(","),
        });
    }

    Ok(())
}

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
use bmc_mock::injection::{Action, Rule, Selector};
use bmc_mock::test_support;
use serde_json::json;

use crate::common;

#[tokio::test]
async fn component_integrity_collection_outcomes_survive_report_conversion() {
    const COLLECTION: &str = "/redfish/v1/ComponentIntegrity";
    const MEMBER: &str = "/redfish/v1/ComponentIntegrity/ERoT_BMC_0";

    for (name, path, action, expected) in [
        (
            "empty collection is a successful answer",
            COLLECTION,
            Action::JsonMerge(json!({"Members": [], "Members@odata.count": 0})),
            (Some(0), false, None),
        ),
        (
            "missing Members is unreadable rather than empty",
            COLLECTION,
            Action::JsonMerge(json!({"Members": null})),
            (None, true, None),
        ),
        (
            "failed member discards an already collected prefix",
            "/redfish/v1/ComponentIntegrity/HGX_ERoT_CPU_1",
            Action::Status(500),
            (None, true, None),
        ),
        (
            "missing enabled flag does not invent a disabled device",
            MEMBER,
            Action::JsonMerge(json!({"ComponentIntegrityEnabled": null})),
            (None, true, None),
        ),
        (
            "unsupported SDK protocol does not produce a fabricated type",
            MEMBER,
            Action::JsonMerge(json!({"ComponentIntegrityType": "FutureProtocol"})),
            (None, true, None),
        ),
        (
            "disabled non-SPDM members remain in inventory",
            MEMBER,
            Action::JsonMerge(
                json!({"ComponentIntegrityType": "TPM", "ComponentIntegrityEnabled": false}),
            ),
            (Some(7), false, Some(("ERoT_BMC_0", "TPM", false))),
        ),
    ] {
        let h = test_support::dgx_gb300_bmc().await;
        h.state.injection.upsert(Rule {
            id: name.into(),
            selector: Selector::OdataId(path.into()),
            action,
            remaining: None,
        });
        let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
            .await
            .unwrap();
        let entries = report.component_integrities.as_deref();
        let actual = (
            entries.map(<[_]>::len),
            report.component_integrity_unavailable,
            entries.and_then(|entries| entries.first()).map(|entry| {
                (
                    entry.id.as_str(),
                    entry.component_integrity_type.as_str(),
                    entry.component_integrity_enabled,
                )
            }),
        );
        assert_eq!(actual, expected, "{name}");
    }
}

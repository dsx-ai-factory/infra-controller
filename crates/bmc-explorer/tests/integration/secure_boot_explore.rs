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
use carbide_instrument::testing::capture_logs_async;
use serde_json::json;

use crate::common;

#[tokio::test]
async fn secure_boot_status_and_warnings() {
    const SYSTEM: &str = "/redfish/v1/Systems/System_0";
    const SECURE_BOOT: &str = "/redfish/v1/Systems/System_0/SecureBoot";
    for (name, path, patch, expected, warning) in [
        ("absent", SYSTEM, json!({"SecureBoot": null}), None, None),
        (
            "enabled",
            SECURE_BOOT,
            json!({"SecureBootEnable": true, "SecureBootCurrentBoot": "Enabled"}),
            Some(true),
            None,
        ),
        (
            "disabled",
            SECURE_BOOT,
            json!({"SecureBootEnable": false, "SecureBootCurrentBoot": "Enabled"}),
            Some(false),
            None,
        ),
        (
            "current boot disabled",
            SECURE_BOOT,
            json!({"SecureBootEnable": true, "SecureBootCurrentBoot": "Disabled"}),
            Some(false),
            None,
        ),
        (
            "missing enable",
            SECURE_BOOT,
            json!({"SecureBootEnable": null}),
            None,
            Some("SecureBootEnable in SecureBoot resource"),
        ),
        (
            "missing current boot",
            SECURE_BOOT,
            json!({"SecureBootCurrentBoot": null}),
            None,
            Some("SecureBootCurrentBootType in SecureBoot resource"),
        ),
    ] {
        let h = test_support::dgx_gb300_bmc().await;
        h.state.injection.upsert(Rule {
            id: name.into(),
            selector: Selector::OdataId(path.into()),
            action: Action::JsonMerge(patch),
            remaining: None,
        });
        let root = h.service_root.as_ref().clone().restrict_expand().into();
        let (result, logs) = capture_logs_async(nv_generate_exploration_report(
            root,
            &common::explorer_config(),
        ))
        .await;
        let report = result.unwrap();
        assert_eq!(
            report.secure_boot_status.map(|s| s.is_enabled),
            expected,
            "{name}"
        );
        let warnings: Vec<_> = logs
            .iter()
            .filter(|log| {
                log.level == tracing::Level::WARN
                    && log.message == "Failed to fetch forge secure boot status."
            })
            .collect();
        assert_eq!(
            warnings.len(),
            usize::from(warning.is_some()),
            "{name}: {logs:?}"
        );
        if let Some(field) = warning {
            assert!(
                warnings[0].field("error").unwrap().contains(field),
                "{name}"
            );
        }
        let secure_boot_diff = report
            .machine_setup_status
            .as_ref()
            .is_some_and(|status| status.diffs.iter().any(|diff| diff.key == "SecureBoot"));
        assert_eq!(secure_boot_diff, expected == Some(true), "{name}");
    }
}

#[tokio::test]
async fn advertised_secure_boot_fetch_failure_fails_exploration() {
    for status in [404, 500] {
        let h = test_support::dgx_gb300_bmc().await;
        h.state.injection.upsert(Rule {
            id: "secure-boot-fetch".into(),
            selector: Selector::OdataId("/redfish/v1/Systems/System_0/SecureBoot".into()),
            action: Action::Status(status),
            remaining: None,
        });
        let result =
            nv_generate_exploration_report(h.service_root, &common::explorer_config()).await;
        assert!(
            matches!(
                result,
                Err(bmc_explorer::Error::NvRedfish {
                    context: "secure boot",
                    ..
                })
            ),
            "HTTP {status}: {result:?}"
        );
    }
}

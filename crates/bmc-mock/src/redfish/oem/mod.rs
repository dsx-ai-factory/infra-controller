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

pub(crate) mod dell;
pub(crate) mod nvidia;
pub(crate) mod supermicro;

use crate::redfish::{ModelExt, Resource};

#[derive(Clone, Copy, Debug)]
pub(crate) enum BmcVendor {
    Dell,
    Nvidia(NvidiaNamestyle),
    Wiwynn,
    LiteOn,
    Delta,
    Ami,
    Supermicro,
    Hpe,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum NvidiaNamestyle {
    Uppercase,
    Capitalized,
}

impl BmcVendor {
    pub(crate) fn service_root_value(&self) -> Option<&'static str> {
        match self {
            BmcVendor::Nvidia(NvidiaNamestyle::Capitalized) => Some("Nvidia"),
            BmcVendor::Nvidia(NvidiaNamestyle::Uppercase) => Some("NVIDIA"),
            BmcVendor::Dell => Some("Dell"),
            BmcVendor::Wiwynn => Some("WIWYNN"),
            BmcVendor::LiteOn => None,
            // Delta power shelves report no `Vendor` in the service root, which
            // is what leads nv-redfish to fall back to its anonymous-BMC quirk.
            BmcVendor::Delta => None,
            BmcVendor::Ami => Some("AMI"),
            BmcVendor::Supermicro => Some("Supermicro"),
            BmcVendor::Hpe => Some("HPE"),
        }
    }
    // This function creates settings of the resource from the resource
    // id. Real identifier is different for different BMC vendors.
    pub(crate) fn make_settings_odata_id(&self, resource: &Resource<'_>) -> String {
        match self {
            // Supermicro uses `{odata_id}/Settings` per the SMC GB300 tray scrape
            // (`/Systems/System_0/Settings`, `/Bios/Settings`). Other Supermicro models
            // may differ; this is scoped to the GB300 tray mock.
            BmcVendor::Nvidia(_)
            | BmcVendor::Dell
            | BmcVendor::Wiwynn
            | BmcVendor::LiteOn
            | BmcVendor::Delta
            | BmcVendor::Supermicro => {
                format!("{}/Settings", resource.odata_id)
            }
            BmcVendor::Ami => {
                format!("{}/SD", resource.odata_id)
            }
            BmcVendor::Hpe => {
                format!("{}/settings", resource.odata_id)
            }
        }
    }
}

#[derive(Clone)]
pub(crate) enum State {
    NvidiaBluefield(nvidia::bluefield::BluefieldState),
    DellIdrac(dell::idrac::IdracState),
    Supermicro(supermicro::manager::SupermicroState),
    Other,
}

/// Add the firmware's type discriminator to a generated OEM complex value.
/// The server generator emits OData metadata only for entity types.
pub(crate) fn object(
    odata_type: &'static str,
    properties: impl serde::Serialize,
) -> serde_json::Value {
    #[derive(serde::Serialize)]
    struct Object<T> {
        #[serde(rename = "@odata.type")]
        odata_type: &'static str,
        #[serde(flatten)]
        properties: T,
    }
    Object {
        odata_type,
        properties,
    }
    .into_json()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::test_support::{TestBmcConfig, create_test_bmc, host_info};
    use crate::{BmcEvent, HardwareType, MachineRouterOptions};

    async fn get(router: &Router, uri: &str) -> Value {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn supermicro_links_serve_and_preserve_patched_oem_state() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::SupermicroGb300Nvl),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        let manager = get(&router, "/redfish/v1/Managers/BMC_0").await;
        for (resource, property, initial, changed) in [
            (
                "KCSInterface",
                "Privilege",
                json!("Administrator"),
                json!("Operator"),
            ),
            (
                "SysLockdown",
                "SysLockdownEnabled",
                json!(false),
                json!(true),
            ),
        ] {
            let uri = manager["Oem"]["Supermicro"][resource]["@odata.id"]
                .as_str()
                .unwrap();
            let before = get(&router, uri).await;
            assert_eq!(before[property], initial);
            assert_eq!(before["@odata.id"], uri);
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(json!({property: changed}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(get(&router, uri).await[property], changed);
        }
    }

    #[tokio::test]
    async fn dell_job_location_tracks_completion_and_preserves_legacy_fields() {
        let (router, state) = create_test_bmc(
            &host_info(HardwareType::DellPowerEdgeR750),
            TestBmcConfig::default(),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        );
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/redfish/v1/Managers/iDRAC.Embedded.1/Jobs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let uri = response.headers()["location"].to_str().unwrap();
        let scheduled = get(&router, uri).await;
        assert_eq!(scheduled["JobState"], "Scheduled");
        assert_eq!(scheduled["PercentComplete"], 0);
        assert_eq!(scheduled["JobType"], "DellConfiguration");
        assert_eq!(scheduled["MessageArgs@odata.count"], 0);
        for property in [
            "ActualRunningStopTime",
            "CompletionTime",
            "TargetSettingsURI",
        ] {
            assert_eq!(scheduled.get(property), Some(&Value::Null));
        }
        state.on_event(&BmcEvent::PowerOn);
        let completed = get(&router, uri).await;
        assert_eq!(completed["JobState"], "Completed");
        assert_eq!(completed["Message"], "Completed");
        assert_eq!(completed["PercentComplete"], 100);
    }
}

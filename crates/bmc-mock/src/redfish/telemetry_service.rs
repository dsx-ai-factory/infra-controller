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

use std::borrow::Cow;
use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::{JsonExt, JsonPatch};
use crate::{Callbacks, http, redfish};

/// Id of the aggregated report the mock publishes.
const REPORT_ID: &str = "PlatformEnvironmentMetrics";

/// Id of a second report whose values the NVIDIA OEM extension marks as
/// carried over from a previous interval.
///
/// Real platforms publish these when a sensing interval elapses without
/// fresh readings; consumers are expected to notice and not treat the
/// repeated numbers as new samples.
const STALE_REPORT_ID: &str = "StaleEnvironmentMetrics";

/// Interval the mock claims its readings are sampled at.
const SENSING_INTERVAL_MS: u32 = 1000;

pub(super) fn resource() -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/TelemetryService"),
        odata_type: Cow::Borrowed("#TelemetryService.v1_3_1.TelemetryService"),
        id: Cow::Borrowed("TelemetryService"),
        name: Cow::Borrowed("Telemetry Service"),
    }
}

fn metric_reports_collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/TelemetryService/MetricReports"),
        odata_type: Cow::Borrowed("#MetricReportCollection.MetricReportCollection"),
        name: Cow::Borrowed("Metric Report Collection"),
    }
}

fn metric_report_resource<'a>(report_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("{}/{report_id}", metric_reports_collection().odata_id);
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#MetricReport.v1_5_0.MetricReport"),
        id: Cow::Borrowed(report_id),
        name: Cow::Borrowed("Metric Report"),
    }
}

fn metric_definitions_collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/TelemetryService/MetricDefinitions"),
        odata_type: Cow::Borrowed("#MetricDefinitionCollection.MetricDefinitionCollection"),
        name: Cow::Borrowed("Metric Definition Collection"),
    }
}

fn metric_definition_resource<'a>(definition_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/{definition_id}",
        metric_definitions_collection().odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#MetricDefinition.v1_3_3.MetricDefinition"),
        id: Cow::Borrowed(definition_id),
        name: Cow::Borrowed("Metric Definition"),
    }
}

pub(crate) fn add_routes<C: Callbacks>(r: Router<BmcState<C>>) -> Router<BmcState<C>> {
    const REPORT_ID_PARAM: &str = "{report_id}";
    const DEFINITION_ID_PARAM: &str = "{definition_id}";
    r.route(&resource().odata_id, get(get_telemetry_service))
        .route(
            &metric_reports_collection().odata_id,
            get(get_metric_reports),
        )
        .route(
            &metric_report_resource(REPORT_ID_PARAM).odata_id,
            get(get_metric_report::<C>),
        )
        .route(
            &metric_definitions_collection().odata_id,
            get(get_metric_definitions::<C>),
        )
        .route(
            &metric_definition_resource(DEFINITION_ID_PARAM).odata_id,
            get(get_metric_definition::<C>),
        )
}

async fn get_telemetry_service() -> Response {
    resource()
        .json_patch()
        .patch(json!({
            "Status": redfish::resource::Status::Ok.into_json(),
            "ServiceEnabled": true,
        }))
        .patch(metric_reports_collection().nav_property("MetricReports"))
        .patch(metric_definitions_collection().nav_property("MetricDefinitions"))
        .into_ok_response()
}

async fn get_metric_reports() -> Response {
    let members = [
        metric_report_resource(REPORT_ID).entity_ref(),
        metric_report_resource(STALE_REPORT_ID).entity_ref(),
    ];
    metric_reports_collection()
        .with_members(&members)
        .into_ok_response()
}

async fn get_metric_report<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(report_id): Path<String>,
) -> Response {
    let stale = match report_id.as_str() {
        REPORT_ID => false,
        STALE_REPORT_ID => true,
        _ => return http::not_found(),
    };

    let timestamp = chrono::Utc::now().to_rfc3339();
    let metric_values: Vec<_> = sensor_metric_values(&state, &timestamp).collect();

    metric_report_resource(&report_id)
        .json_patch()
        .patch(json!({
            "Timestamp": timestamp,
            "MetricValues@odata.count": metric_values.len(),
            "MetricValues": metric_values,
            "Oem": {
                "Nvidia": {
                    "@odata.type": "#NvidiaMetricReport.v1_0_0.NvidiaMetricReport",
                    "SensingIntervalMilliseconds": SENSING_INTERVAL_MS,
                    "MetricValueStale": stale,
                }
            }
        }))
        .into_ok_response()
}

async fn get_metric_definitions<C: Callbacks>(State(state): State<BmcState<C>>) -> Response {
    let paths: Vec<_> = metric_definition_units(&state)
        .into_keys()
        .map(|id| metric_definition_resource(&id).entity_ref())
        .collect();
    metric_definitions_collection()
        .with_members(&paths)
        .into_ok_response()
}

async fn get_metric_definition<C: Callbacks>(
    State(state): State<BmcState<C>>,
    Path(definition_id): Path<String>,
) -> Response {
    let Some(units) = metric_definition_units(&state).remove(&definition_id) else {
        return http::not_found();
    };

    metric_definition_resource(&definition_id)
        .json_patch()
        .patch(json!({
            "MetricType": "Numeric",
            "MetricDataType": "Decimal",
            "Units": units,
            "Implementation": "PhysicalSensor",
        }))
        .into_ok_response()
}

/// The distinct `MetricId`s the reports use, mapped to the unit the
/// underlying sensor reads in.
///
/// One definition covers every property it applies to, so the same
/// sensor id on several chassis collapses to a single entry here while
/// still producing one `MetricValue` per chassis in the report. That is
/// the shape real platforms publish, and the reason `MetricProperty`
/// rather than `MetricId` is what identifies a reading.
fn metric_definition_units<C: Callbacks>(state: &BmcState<C>) -> BTreeMap<String, String> {
    sensors(state)
        .filter_map(|(_, sensor)| {
            let units = sensor
                .to_json()
                .get("ReadingUnits")
                .and_then(serde_json::Value::as_str)?
                .to_string();
            Some((sensor.id.to_string(), units))
        })
        .collect()
}

fn sensors<C: Callbacks>(
    state: &BmcState<C>,
) -> impl Iterator<Item = (&str, &redfish::sensor::Sensor)> {
    state.chassis_state.iter().flat_map(|chassis| {
        let id = chassis.config.id.as_ref();
        chassis
            .config
            .sensors
            .iter()
            .flatten()
            .map(move |sensor| (id, sensor))
    })
}

fn sensor_metric_values<'a, C: Callbacks>(
    state: &'a BmcState<C>,
    timestamp: &'a str,
) -> impl Iterator<Item = serde_json::Value> + 'a {
    sensors(state).filter_map(move |(chassis_id, sensor)| {
        let reading = sensor.to_json().get("Reading")?.as_f64()?;
        let odata_id = redfish::sensor::chassis_resource(chassis_id, &sensor.id).odata_id;
        Some(json!({
            "MetricId": sensor.id,
            "MetricValue": reading.to_string(),
            "MetricProperty": odata_id,
            "Timestamp": timestamp,
        }))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
    use serde_json::json;
    use url::Url;

    use super::{REPORT_ID, SENSING_INTERVAL_MS, STALE_REPORT_ID};
    use crate::test_support::axum_http_client::AxumRouterHttpClient;
    use crate::test_support::{NoopCallbacks, TEST_MAC_POOL};
    use crate::{
        DpuMachineInfo, DpuSettings, HardwareType, HostMachineInfo, MachineInfo,
        MachineRouterOptions, machine_router,
    };

    fn test_host_mock() -> Router {
        let mut mac_pool = TEST_MAC_POOL.lock().unwrap();
        let hw_type = HardwareType::DellPowerEdgeR750;
        let ranges_config = mac_pool.allocate_range_config().unwrap();

        machine_router(
            &MachineInfo::Host(HostMachineInfo::new(
                hw_type,
                vec![DpuMachineInfo::new(
                    hw_type,
                    &mut mac_pool,
                    DpuSettings::default(),
                )],
                &mut mac_pool,
                ranges_config,
            )),
            Arc::new(NoopCallbacks),
            "test-host-id".to_string(),
            false,
            MachineRouterOptions::default(),
        )
        .0
    }

    async fn get(
        client: &AxumRouterHttpClient,
        path: &str,
    ) -> Result<serde_json::Value, impl std::error::Error> {
        let url = Url::parse(&format!("https://bmc-mock.local{path}")).expect("valid URL");
        client
            .get(
                url,
                &BmcCredentials::new("root".to_string(), "password".to_string()),
                None,
                &axum::http::HeaderMap::new(),
            )
            .await
    }

    #[tokio::test]
    async fn telemetry_service_serves_sensor_readings_as_metric_report() {
        let router = test_host_mock();
        let client = AxumRouterHttpClient::new(router);

        let reports = "/redfish/v1/TelemetryService/MetricReports";

        // Service root advertises the service, which links the reports collection.
        let root = get(&client, "/redfish/v1").await.unwrap();
        assert_eq!(
            root["TelemetryService"]["@odata.id"],
            "/redfish/v1/TelemetryService"
        );
        let service = get(&client, "/redfish/v1/TelemetryService").await.unwrap();
        assert_eq!(service["ServiceEnabled"], true);
        assert_eq!(service["MetricReports"]["@odata.id"], reports);

        // The collection lists the aggregated report and its stale twin.
        let collection = get(&client, reports).await.unwrap();
        assert_eq!(
            collection["Members"],
            json!([
                { "@odata.id": format!("{reports}/{REPORT_ID}") },
                { "@odata.id": format!("{reports}/{STALE_REPORT_ID}") },
            ])
        );

        // Every value mirrors a chassis sensor reading.
        let report = get(&client, &format!("{reports}/{REPORT_ID}"))
            .await
            .unwrap();
        let values = report["MetricValues"].as_array().expect("MetricValues");
        assert!(!values.is_empty(), "report should mirror chassis sensors");
        assert_eq!(report["MetricValues@odata.count"], values.len());
        for value in values {
            value["MetricValue"]
                .as_str()
                .expect("MetricValue string")
                .parse::<f64>()
                .expect("numeric reading");
        }

        // Unknown report ids 404.
        assert!(get(&client, &format!("{reports}/Nope")).await.is_err());
    }

    #[tokio::test]
    async fn metric_reports_carry_the_nvidia_oem_extension() {
        let client = AxumRouterHttpClient::new(test_host_mock());
        let reports = "/redfish/v1/TelemetryService/MetricReports";

        let fresh = get(&client, &format!("{reports}/{REPORT_ID}"))
            .await
            .unwrap();
        let oem = &fresh["Oem"]["Nvidia"];
        assert_eq!(oem["SensingIntervalMilliseconds"], SENSING_INTERVAL_MS);
        assert_eq!(oem["MetricValueStale"], false);

        let stale = get(&client, &format!("{reports}/{STALE_REPORT_ID}"))
            .await
            .unwrap();
        assert_eq!(stale["Oem"]["Nvidia"]["MetricValueStale"], true);
    }

    #[tokio::test]
    async fn metric_definitions_declare_units_for_every_metric_id() {
        let client = AxumRouterHttpClient::new(test_host_mock());
        let definitions = "/redfish/v1/TelemetryService/MetricDefinitions";

        let service = get(&client, "/redfish/v1/TelemetryService").await.unwrap();
        assert_eq!(service["MetricDefinitions"]["@odata.id"], definitions);

        let collection = get(&client, definitions).await.unwrap();
        let members = collection["Members"].as_array().expect("Members");
        assert!(!members.is_empty(), "sensors should define metrics");

        // Every MetricId used by the report resolves to a definition
        // that declares a unit -- that pairing is what lets a consumer
        // put units on a reading the report itself does not carry.
        let report = get(
            &client,
            &format!("/redfish/v1/TelemetryService/MetricReports/{REPORT_ID}"),
        )
        .await
        .unwrap();
        for value in report["MetricValues"].as_array().expect("MetricValues") {
            let metric_id = value["MetricId"].as_str().expect("MetricId");
            let definition = get(&client, &format!("{definitions}/{metric_id}"))
                .await
                .unwrap_or_else(|_| panic!("{metric_id} should have a definition"));
            assert_eq!(definition["Id"], metric_id);
            assert!(
                definition["Units"].is_string(),
                "{metric_id} definition should declare units"
            );
        }

        // Unknown definition ids 404.
        assert!(get(&client, &format!("{definitions}/Nope")).await.is_err());
    }
}

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

use std::sync::Arc;
use std::time::{Duration, Instant};

use nv_redfish::ServiceRoot;
use nv_redfish::core::{Bmc, EntityTypeRef, ODataId, ToSnakeCase};
use nv_redfish::resource::State;
use nv_redfish::schema::leak_detector::{DetectorState, LeakDetector};

use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::sink::{
    Classification, CollectorEvent, DataSink, EventContext, HealthReport, HealthReportAlert,
    HealthReportSuccess, Probe, ReportSource,
};

pub struct LeakDetectorCollectorConfig {
    pub data_sink: Option<Arc<dyn DataSink>>,
    pub state_refresh_interval: Duration,
}

pub struct LeakDetectorCollector<B: Bmc> {
    bmc: Arc<B>,
    event_context: EventContext,
    state: Option<LeakDetectorCollectorState>,
    data_sink: Option<Arc<dyn DataSink>>,
    state_refresh_interval: Duration,
}

struct LeakDetectorCollectorState {
    detector_ids: Vec<ODataId>,
    last_detector_refresh: Instant,
}

impl<B> PeriodicCollector<B> for LeakDetectorCollector<B>
where
    B: Bmc + 'static,
    B::Error: 'static,
{
    type Config = LeakDetectorCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context =
            EventContext::from_endpoint(endpoint.as_ref(), "leak_detector_collector");
        Ok(Self {
            bmc,
            event_context,
            state: None,
            data_sink: config.data_sink,
            state_refresh_interval: config.state_refresh_interval,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.run_leak_detector_iteration().await
    }

    fn collector_type(&self) -> &'static str {
        "leak_detector_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B> LeakDetectorCollector<B>
where
    B: Bmc + 'static,
    B::Error: 'static,
{
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    async fn run_leak_detector_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let needs_detector_refresh = self
            .state
            .as_ref()
            .map(|s| s.last_detector_refresh.elapsed() > self.state_refresh_interval)
            .unwrap_or(true);

        let mut refresh_triggered = false;

        if needs_detector_refresh {
            match self.discover_leak_detectors().await {
                Ok(detector_ids) => {
                    tracing::info!(
                        detector_count = detector_ids.len(),
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Leak detector discovery complete"
                    );
                    self.state = Some(LeakDetectorCollectorState {
                        detector_ids,
                        last_detector_refresh: Instant::now(),
                    });
                    refresh_triggered = true;
                }
                Err(error) => {
                    tracing::error!(
                        ?error,
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Failed to discover leak detectors"
                    );

                    if self.state.is_none() {
                        return Err(error);
                    }
                }
            }
        }

        let (detectors, unreadable) = if let Some(state) = &self.state {
            self.fetch_leak_detectors(&state.detector_ids).await
        } else {
            (Vec::new(), Vec::new())
        };
        let detector_count = detectors.len();
        let fetch_failures = unreadable.len();
        let report = build_health_report(detectors, &unreadable, &self.event_context);

        self.emit_event(CollectorEvent::HealthReport(Arc::new(report)));

        Ok(IterationResult {
            refresh_triggered,
            entity_count: Some(detector_count),
            fetch_failures,
        })
    }

    async fn discover_leak_detectors(&self) -> Result<Vec<ODataId>, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let Some(chassis_collection) = service_root.chassis().await? else {
            return Ok(Vec::new());
        };

        let mut detector_ids = Vec::new();
        for chassis in chassis_collection.members().await? {
            // These are optional Redfish navigation properties. Each link must
            // be fetched before the next one exists, so this stays as an
            // explicit step-by-step walk instead of an Option chain.
            let Some(thermal_subsystem_ref) = &chassis.raw().thermal_subsystem else {
                continue;
            };
            let thermal_subsystem = thermal_subsystem_ref
                .get(self.bmc.as_ref())
                .await
                .map_err(|error| HealthError::BmcError(Box::new(error)))?;
            let Some(leak_detection_ref) = &thermal_subsystem.leak_detection else {
                continue;
            };
            let leak_detection = leak_detection_ref
                .get(self.bmc.as_ref())
                .await
                .map_err(|error| HealthError::BmcError(Box::new(error)))?;
            let Some(leak_detector_collection_ref) = &leak_detection.leak_detectors else {
                continue;
            };
            let leak_detector_collection = leak_detector_collection_ref
                .get(self.bmc.as_ref())
                .await
                .map_err(|error| HealthError::BmcError(Box::new(error)))?;

            for leak_detector_ref in &leak_detector_collection.members {
                detector_ids.push(leak_detector_ref.id().clone());
            }
        }

        Ok(detector_ids)
    }

    /// Fetches every discovered detector, returning the ones that could be read
    /// along with the ids of the ones that could not.
    ///
    /// A detector the BMC serves in a shape this client cannot decode costs its
    /// own leak reporting instead of every other detector on the endpoint. It is
    /// reported as unreadable rather than dropped: leak classification counts
    /// alerts, so a silently shorter batch would read downstream as an all-clear
    /// for a detector whose state is actually unknown. The count also reaches
    /// the collector runtime as `fetch_failures`, which is the operator-visible
    /// signal that this endpoint is reporting on fewer detectors than it
    /// discovered.
    async fn fetch_leak_detectors(
        &self,
        detector_ids: &[ODataId],
    ) -> (Vec<Arc<LeakDetector>>, Vec<ODataId>) {
        let mut detectors = Vec::new();
        let mut unreadable = Vec::new();
        for detector_id in detector_ids {
            match self.bmc.get::<LeakDetector>(detector_id).await {
                Ok(detector) => detectors.push(detector),
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        detector = %detector_id,
                        "Failed to fetch leak detector; it is reported as unreadable"
                    );
                    unreadable.push(detector_id.clone());
                }
            }
        }

        (detectors, unreadable)
    }
}

fn build_health_report(
    detectors: Vec<Arc<LeakDetector>>,
    unreadable: &[ODataId],
    context: &EventContext,
) -> HealthReport {
    let mut successes = Vec::new();
    let mut alerts = Vec::new();

    for detector in detectors {
        let target = detector_target(&detector);
        let resource_state = detector
            .status
            .as_ref()
            .and_then(|status| status.state.flatten());
        if resource_state != Some(State::Enabled) {
            tracing::warn!(
                detector = %target,
                leak_detector_state = ?detector.detector_state.flatten(),
                leak_detector_resource_state = ?resource_state,
                rack_id = context.rack_id().map(tracing::field::display),
                "Leak detector resource state does not permit leak classification"
            );
            continue;
        }

        match detector.detector_state.flatten() {
            Some(DetectorState::Ok) => successes.push(HealthReportSuccess {
                probe_id: Probe::LeakDetection,
                target: Some(target),
            }),
            Some(DetectorState::Warning) | Some(DetectorState::Critical) => {
                alerts.push(leak_alert(&detector, target));
            }
            Some(DetectorState::Unavailable)
            | Some(DetectorState::Absent)
            | Some(DetectorState::UnsupportedValue)
            | None => {
                tracing::warn!(
                    detector = %target,
                    leak_detector_state = ?detector.detector_state.flatten(),
                    rack_id = context.rack_id().map(tracing::field::display),
                    "Leak detector is not reporting an actionable leak state"
                );
            }
        }
    }

    // A detector this client could not read has unknown state, not healthy
    // state. Reporting it as a sensor failure keeps that distinction visible to
    // leak classification, which would otherwise see only a shorter batch.
    for detector_id in unreadable {
        alerts.push(HealthReportAlert {
            probe_id: Probe::LeakDetection,
            target: Some(detector_id.to_string()),
            message: format!("Leak detector '{detector_id}' could not be read"),
            classifications: vec![Classification::SensorFailure],
        });
    }

    HealthReport {
        source: ReportSource::BmcLeakDetectors,
        observed_at: Some(chrono::Utc::now()),
        successes,
        alerts,
        target: context.health_report_target(),
    }
}

fn detector_target(detector: &LeakDetector) -> String {
    detector
        .user_label
        .clone()
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| detector.odata_id().to_string())
}

fn leak_alert(detector: &LeakDetector, target: String) -> HealthReportAlert {
    let state = detector.detector_state.flatten();
    HealthReportAlert {
        probe_id: Probe::LeakDetection,
        target: Some(target.clone()),
        message: format!(
            "Leak detector '{}' reports {}",
            target,
            state
                .map(|state| state.to_snake_case())
                .unwrap_or("unknown")
        ),
        classifications: vec![Classification::LeakDetector],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use axum::Router;
    use axum::extract::{OriginalUri, State};
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use bmc_mock::test_support::TestBmc;
    use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;
    use nv_redfish::bmc_http::{BmcCredentials, CacheSettings};
    use url::Url;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
    use crate::sink::HealthReportTarget;

    const DETECTORS_PATH: &str =
        "/redfish/v1/Chassis/MGX_BMC_0/ThermalSubsystem/LeakDetection/LeakDetectors";

    /// A detector body in the shape a real VR RTF switch BMC serves, with
    /// `ReactionDelaySeconds` interpolated verbatim so a caller chooses the
    /// JSON encoding the client has to parse.
    fn switch_detector_body(id: &str, reaction_delay_seconds: &str) -> String {
        format!(
            r##"{{
                "@odata.id": "{DETECTORS_PATH}/{id}",
                "@odata.type": "#LeakDetector.v1_4_0.LeakDetector",
                "CriticalReactionType": "None",
                "DetectorState": "OK",
                "Id": "{id}",
                "LeakDetectorType": "Moisture",
                "Name": "{id}",
                "ReactionDelaySeconds": {reaction_delay_seconds},
                "Status": {{ "State": "Enabled" }},
                "WarningReactionType": "None"
            }}"##
        )
    }

    #[derive(Debug, Eq, PartialEq)]
    struct SuccessSummary {
        probe_id: Probe,
        target: Option<String>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct AlertSummary {
        probe_id: Probe,
        target: Option<String>,
        message: String,
        classifications: Vec<Classification>,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ReportSummary {
        source: ReportSource,
        target: Option<HealthReportTarget>,
        has_observed_at: bool,
        successes: Vec<SuccessSummary>,
        alerts: Vec<AlertSummary>,
    }

    impl From<HealthReport> for ReportSummary {
        fn from(report: HealthReport) -> Self {
            Self {
                source: report.source,
                target: report.target,
                has_observed_at: report.observed_at.is_some(),
                successes: report
                    .successes
                    .into_iter()
                    .map(|success| SuccessSummary {
                        probe_id: success.probe_id,
                        target: success.target,
                    })
                    .collect(),
                alerts: report
                    .alerts
                    .into_iter()
                    .map(|alert| AlertSummary {
                        probe_id: alert.probe_id,
                        target: alert.target,
                        message: alert.message,
                        classifications: alert.classifications,
                    })
                    .collect(),
            }
        }
    }

    fn context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "leak_detector_collector",
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: None,
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn expected_report(successes: Vec<SuccessSummary>, alerts: Vec<AlertSummary>) -> ReportSummary {
        ReportSummary {
            source: ReportSource::BmcLeakDetectors,
            target: Some(HealthReportTarget::Machine),
            has_observed_at: true,
            successes,
            alerts,
        }
    }

    #[test]
    fn leak_detector_health_report_cases() {
        check_values(
            [
                Check {
                    scenario: "no detectors produce an empty timestamped report",
                    input: vec![],
                    expect: expected_report(vec![], vec![]),
                },
                Check {
                    scenario: "actionable states preserve alert order and detector targets",
                    input: vec![
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Critical",
                            "Id": "Critical",
                            "Name": "Critical leak detector",
                            "DetectorState": "Critical",
                            "Status": { "Health": "Critical", "State": "Enabled" },
                            "UserLabel": ""
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/OK",
                            "Id": "OK",
                            "Name": "Healthy leak detector",
                            "DetectorState": "OK",
                            "Status": { "Health": "OK", "State": "Enabled" },
                            "UserLabel": "Rack floor"
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Warning",
                            "Id": "Warning",
                            "Name": "Warning leak detector",
                            "DetectorState": "Warning",
                            "Status": { "Health": "Warning", "State": "Enabled" },
                            "UserLabel": "Cooling tray"
                        }"#,
                    ],
                    expect: expected_report(
                        vec![SuccessSummary {
                            probe_id: Probe::LeakDetection,
                            target: Some("Rack floor".to_string()),
                        }],
                        vec![
                            AlertSummary {
                                probe_id: Probe::LeakDetection,
                                target: Some(
                                    "/redfish/v1/Chassis/System/LeakDetectors/Critical".to_string(),
                                ),
                                message: "Leak detector '/redfish/v1/Chassis/System/LeakDetectors/Critical' reports critical".to_string(),
                                classifications: vec![Classification::LeakDetector],
                            },
                            AlertSummary {
                                probe_id: Probe::LeakDetection,
                                target: Some("Cooling tray".to_string()),
                                message: "Leak detector 'Cooling tray' reports warning".to_string(),
                                classifications: vec![Classification::LeakDetector],
                            },
                        ],
                    ),
                },
                Check {
                    scenario: "degraded leak detectors do not produce leak alerts",
                    input: vec![
                        r##"{
                            "@odata.id": "/redfish/v1/Chassis/Chassis_0/ThermalSubsystem/LeakDetection/LeakDetectors/Chassis_0_LeakDetector_0_ColdPlate",
                            "@odata.type": "#LeakDetector.v1_1_0.LeakDetector",
                            "Id": "Chassis_0_LeakDetector_0_ColdPlate",
                            "Name": "Chassis 0 LeakDetector 0 ColdPlate",
                            "DetectorState": "Critical",
                            "LeakDetectorType": "Moisture",
                            "Status": { "Health": "Critical", "State": "Degraded" }
                        }"##,
                    ],
                    expect: expected_report(vec![], vec![]),
                },
                Check {
                    scenario: "missing and disabled resource states do not produce leak alerts",
                    input: vec![
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/MissingStatus",
                            "Id": "MissingStatus",
                            "Name": "Missing status leak detector",
                            "DetectorState": "Critical"
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/MissingResourceState",
                            "Id": "MissingResourceState",
                            "Name": "Missing resource state leak detector",
                            "DetectorState": "Critical",
                            "Status": { "Health": "Critical" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Disabled",
                            "Id": "Disabled",
                            "Name": "Disabled leak detector",
                            "DetectorState": "Critical",
                            "Status": { "Health": "Critical", "State": "Disabled" }
                        }"#,
                    ],
                    expect: expected_report(vec![], vec![]),
                },
                Check {
                    scenario: "non-actionable and missing states do not produce report entries",
                    input: vec![
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Unavailable",
                            "Id": "Unavailable",
                            "Name": "Unavailable leak detector",
                            "DetectorState": "Unavailable",
                            "Status": { "State": "Enabled" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Absent",
                            "Id": "Absent",
                            "Name": "Absent leak detector",
                            "DetectorState": "Absent",
                            "Status": { "State": "Enabled" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Vendor",
                            "Id": "Vendor",
                            "Name": "Vendor leak detector",
                            "DetectorState": "VendorDefinedState",
                            "Status": { "State": "Enabled" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Missing",
                            "Id": "Missing",
                            "Name": "Missing state leak detector",
                            "Status": { "State": "Enabled" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Null",
                            "Id": "Null",
                            "Name": "Null state leak detector",
                            "DetectorState": null,
                            "Status": { "State": "Enabled" }
                        }"#,
                    ],
                    expect: expected_report(vec![], vec![]),
                },
            ],
            |json_detectors| {
                let detectors = json_detectors
                    .into_iter()
                    .map(|json| {
                        Arc::new(
                            serde_json::from_str::<LeakDetector>(json).expect("valid leak detector"),
                        )
                    })
                    .collect();

                build_health_report(detectors, &[], &context()).into()
            },
        );
    }

    #[test]
    fn unreadable_detectors_report_as_sensor_failures() {
        let readable = r#"{
            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/OK",
            "Id": "OK",
            "Name": "Healthy leak detector",
            "DetectorState": "OK",
            "Status": { "Health": "OK", "State": "Enabled" }
        }"#;
        let detectors = vec![Arc::new(
            serde_json::from_str::<LeakDetector>(readable).expect("valid leak detector"),
        )];
        let unreadable = [ODataId::from(format!("{DETECTORS_PATH}/leakage2"))];

        let report: ReportSummary = build_health_report(detectors, &unreadable, &context()).into();

        assert_eq!(
            report,
            expected_report(
                vec![SuccessSummary {
                    probe_id: Probe::LeakDetection,
                    target: Some("/redfish/v1/Chassis/System/LeakDetectors/OK".to_string()),
                }],
                vec![AlertSummary {
                    probe_id: Probe::LeakDetection,
                    target: Some(format!("{DETECTORS_PATH}/leakage2")),
                    message: format!("Leak detector '{DETECTORS_PATH}/leakage2' could not be read"),
                    classifications: vec![Classification::SensorFailure],
                }],
            ),
            "an unreadable detector must stay visible instead of silently shortening the batch"
        );
    }

    async fn detector_response(
        State(bodies): State<Arc<HashMap<String, String>>>,
        OriginalUri(uri): OriginalUri,
    ) -> Response {
        match bodies.get(uri.path()) {
            Some(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                body.clone(),
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    /// `LeakDetector_v1.xml` declares `ReactionDelaySeconds` as `Edm.Int64`, so
    /// the `0.0` that switch BMC firmware serves for it fails to decode. That is
    /// how a single detector body becomes unreadable in the field, and this
    /// collector never reads the property that sinks it.
    #[tokio::test]
    async fn undecodable_detector_is_named_without_losing_the_batch() {
        let detectors = [("leakage1", "0"), ("leakage2", "0.0"), ("leakage3", "30")];
        let bodies: HashMap<String, String> = detectors
            .iter()
            .map(|(id, delay)| {
                (
                    format!("{DETECTORS_PATH}/{id}"),
                    switch_detector_body(id, delay),
                )
            })
            .collect();
        let router = Router::new()
            .fallback(detector_response)
            .with_state(Arc::new(bodies));
        let bmc = Arc::new(TestBmc::new(
            AxumRouterHttpClient::new(router),
            Url::parse("https://leak-detector-test.local").expect("test URL should parse"),
            BmcCredentials::new("root".to_string(), "password".to_string()),
            CacheSettings::with_capacity(8),
        ));
        let collector = LeakDetectorCollector::new_runner(
            bmc,
            Arc::new(test_endpoint(mac("42:9e:b1:bd:9d:dd"))),
            LeakDetectorCollectorConfig {
                data_sink: None,
                state_refresh_interval: Duration::from_secs(60),
            },
        )
        .expect("leak detector collector should build");

        let detector_ids: Vec<ODataId> = detectors
            .iter()
            .map(|(id, _)| ODataId::from(format!("{DETECTORS_PATH}/{id}")))
            .collect();
        let (fetched, unreadable) = collector.fetch_leak_detectors(&detector_ids).await;

        let fetched_ids: Vec<String> = fetched
            .iter()
            .map(|detector| detector.odata_id().to_string())
            .collect();
        let unreadable_ids: Vec<String> = unreadable.iter().map(ToString::to_string).collect();
        assert_eq!(
            (fetched_ids, unreadable_ids),
            (
                vec![
                    format!("{DETECTORS_PATH}/leakage1"),
                    format!("{DETECTORS_PATH}/leakage3"),
                ],
                vec![format!("{DETECTORS_PATH}/leakage2")],
            ),
            "a detector this client cannot decode should be named, not dropped from the batch"
        );
    }
}

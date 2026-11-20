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

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nv_redfish::ServiceRoot;
use nv_redfish::core::{Bmc, EntityTypeRef, ODataId, ToSnakeCase};
use nv_redfish::resource::State;
use nv_redfish::schema::leak_detector::{DetectorState, LeakDetector};

use crate::HealthError;
use crate::bmc::is_connection_bmc_source_error;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::sink::{
    Classification, CollectorEvent, DataSink, EventContext, HealthReport, HealthReportAlert,
    HealthReportSuccess, Probe, ReportSource,
};

pub struct LeakDetectorCollectorConfig {
    pub data_sink: Option<Arc<dyn DataSink>>,
    pub state_refresh_interval: Duration,
    pub aggregate_detector_ids: AggregateDetectorIds,
}

pub struct LeakDetectorCollector<B: Bmc> {
    bmc: Arc<B>,
    event_context: EventContext,
    state: Option<LeakDetectorCollectorState>,
    data_sink: Option<Arc<dyn DataSink>>,
    state_refresh_interval: Duration,
    aggregate_detector_ids: AggregateDetectorIds,
}

struct LeakDetectorCollectorState {
    detector_ids: Vec<ODataId>,
    last_detector_refresh: Instant,
}

/// Redfish `Id`s of aggregate leak detectors, which roll up the other
/// detectors on their chassis.
#[derive(Clone, Debug, Default)]
pub struct AggregateDetectorIds(BTreeSet<String>);

impl From<BTreeSet<String>> for AggregateDetectorIds {
    fn from(ids: BTreeSet<String>) -> Self {
        Self(ids)
    }
}

impl AggregateDetectorIds {
    /// Whether the last segment of `detector` is exactly a configured id.
    fn matches(&self, detector: &ODataId) -> bool {
        detector
            .last_segment()
            .is_some_and(|id| self.0.contains(id))
    }

    /// Splits one chassis's leak detectors into the ones to report and the
    /// aggregates to ignore. Aggregates are ignored when the chassis also
    /// exposes individual detectors, because a BMC can keep an aggregate
    /// critical after the detectors it rolls up go offline. A chassis that
    /// exposes only aggregates reports them.
    fn split(&self, chassis_detectors: Vec<ODataId>) -> DiscoveredDetectors {
        match chassis_detectors
            .into_iter()
            .partition::<Vec<_>, _>(|detector| self.matches(detector))
        {
            (aggregates, individual) if individual.is_empty() => DiscoveredDetectors {
                reported: aggregates,
                ignored_aggregates: Vec::new(),
            },
            (aggregates, individual) => DiscoveredDetectors {
                reported: individual,
                ignored_aggregates: aggregates,
            },
        }
    }
}

/// What one pass read from the discovered leak detectors.
#[derive(Default)]
struct DetectorReads {
    /// Detectors that were read.
    detectors: Vec<Arc<LeakDetector>>,
    /// Ids and errors of detectors that could not be read.
    unreadable: Vec<(ODataId, String)>,
    /// How many unreadable detectors failed because the BMC was unreachable.
    connection_failures: usize,
}

impl DetectorReads {
    /// The pass's health report, or `None` to keep the previous one. Each
    /// unreadable detector is one of the report's alerts, so the previous
    /// report is kept only when the BMC was unreachable and those connection
    /// failures are all the pass has to say. A leak, an offline detector, or a
    /// decode failure adds an alert of its own and publishes the report,
    /// unreachable detectors included, so it cannot clear a leak on them.
    fn into_report(self, context: &EventContext) -> Option<HealthReport> {
        let report = build_health_report(self.detectors, &self.unreadable, context);
        (self.connection_failures == 0 || report.alerts.len() > self.connection_failures)
            .then_some(report)
    }
}

/// Leak detectors found by discovery.
#[derive(Default)]
struct DiscoveredDetectors {
    /// Detectors polled and reported each iteration.
    reported: Vec<ODataId>,
    /// Aggregate detectors left out because their chassis also exposes
    /// individual detectors.
    ignored_aggregates: Vec<ODataId>,
}

/// Combines per-chassis results into the endpoint's discovery result.
impl FromIterator<DiscoveredDetectors> for DiscoveredDetectors {
    fn from_iter<I: IntoIterator<Item = DiscoveredDetectors>>(chassis: I) -> Self {
        chassis
            .into_iter()
            .fold(Self::default(), |mut endpoint, chassis| {
                endpoint.reported.extend(chassis.reported);
                endpoint
                    .ignored_aggregates
                    .extend(chassis.ignored_aggregates);
                endpoint
            })
    }
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
            aggregate_detector_ids: config.aggregate_detector_ids,
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
                Ok(discovered) => {
                    tracing::info!(
                        detector_count = discovered.reported.len(),
                        ignored_aggregate_detectors = ?discovered
                            .ignored_aggregates
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>(),
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Leak detector discovery complete"
                    );
                    self.state = Some(LeakDetectorCollectorState {
                        detector_ids: discovered.reported,
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

        let reads = match &self.state {
            Some(state) => self.fetch_leak_detectors(&state.detector_ids).await,
            None => DetectorReads::default(),
        };
        let detector_count = reads.detectors.len();
        let fetch_failures = reads.unreadable.len();

        if let Some(report) = reads.into_report(&self.event_context) {
            self.emit_event(CollectorEvent::HealthReport(Arc::new(report)));
        }

        Ok(IterationResult {
            refresh_triggered,
            entity_count: Some(detector_count),
            fetch_failures,
        })
    }

    async fn discover_leak_detectors(&self) -> Result<DiscoveredDetectors, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let Some(chassis_collection) = service_root.chassis().await? else {
            return Ok(DiscoveredDetectors::default());
        };

        let mut chassis_detectors = Vec::new();
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

            chassis_detectors.push(
                leak_detector_collection
                    .members
                    .iter()
                    .map(|leak_detector_ref| leak_detector_ref.id().clone())
                    .collect(),
            );
        }

        Ok(chassis_detectors
            .into_iter()
            .map(|detectors| self.aggregate_detector_ids.split(detectors))
            .collect())
    }

    /// Fetches every discovered detector.
    ///
    /// A detector the BMC serves in a shape this client cannot decode costs its
    /// own leak reporting instead of every other detector on the endpoint. It is
    /// reported as unreadable rather than dropped: leak classification counts
    /// alerts, so a silently shorter batch would read downstream as an all-clear
    /// for a detector whose state is actually unknown. The count also reaches
    /// the collector runtime as `fetch_failures`, which is the operator-visible
    /// signal that this endpoint is reporting on fewer detectors than it
    /// discovered.
    async fn fetch_leak_detectors(&self, detector_ids: &[ODataId]) -> DetectorReads {
        let mut reads = DetectorReads::default();
        for detector_id in detector_ids {
            match self.bmc.get::<LeakDetector>(detector_id).await {
                Ok(detector) => reads.detectors.push(detector),
                Err(error) => {
                    reads.connection_failures +=
                        usize::from(is_connection_bmc_source_error(&error));
                    tracing::warn!(?error, detector = %detector_id, "Failed to fetch leak detector");
                    reads
                        .unreadable
                        .push((detector_id.clone(), error.to_string()));
                }
            }
        }
        reads
    }
}

fn build_health_report(
    detectors: Vec<Arc<LeakDetector>>,
    unreadable: &[(ODataId, String)],
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
        match resource_state {
            Some(State::Enabled) => {}
            // An offline detector cannot sense a leak. With its chassis's
            // aggregate ignored, this alert is the only signal that the tray
            // has lost leak coverage, and it keeps leak classification from
            // reading the tray as dry.
            Some(state @ (State::UnavailableOffline | State::StandbyOffline)) => {
                alerts.push(offline_alert(target, state));
                continue;
            }
            _ => {
                tracing::warn!(
                    detector = %target,
                    leak_detector_state = ?detector.detector_state.flatten(),
                    leak_detector_resource_state = ?resource_state,
                    rack_id = context.rack_id().map(tracing::field::display),
                    "Leak detector resource state does not permit leak classification"
                );
                continue;
            }
        }

        match detector.detector_state.flatten() {
            Some(DetectorState::Ok) => successes.push(HealthReportSuccess {
                attribution: None,
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
    for (detector_id, error) in unreadable {
        alerts.push(HealthReportAlert {
            attribution: None,
            probe_id: Probe::LeakDetection,
            target: Some(detector_id.to_string()),
            message: format!("Leak detector '{detector_id}' could not be read: {error}"),
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

fn offline_alert(target: String, state: State) -> HealthReportAlert {
    HealthReportAlert {
        attribution: None,
        probe_id: Probe::LeakDetection,
        message: format!(
            "Leak detector '{target}' is offline: {}",
            state.to_snake_case()
        ),
        target: Some(target),
        classifications: vec![Classification::SensorFailure],
    }
}

fn leak_alert(detector: &LeakDetector, target: String) -> HealthReportAlert {
    let state = detector.detector_state.flatten();
    HealthReportAlert {
        attribution: None,
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
    use std::sync::Mutex;

    use axum::Router;
    use axum::extract::{OriginalUri, State};
    use axum::http::{StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use bmc_mock::test_support::TestBmc;
    use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use mac_address::MacAddress;
    use nv_redfish::bmc_http::{BmcCredentials, CacheSettings};
    use serde_json::json;
    use url::Url;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
    use crate::sink::HealthReportTarget;

    const DETECTORS_PATH: &str =
        "/redfish/v1/Chassis/MGX_BMC_0/ThermalSubsystem/LeakDetection/LeakDetectors";
    const WIWYNN_CHASSIS_PATH: &str = "/redfish/v1/Chassis/Chassis_0";
    const WIWYNN_DETECTORS_PATH: &str =
        "/redfish/v1/Chassis/Chassis_0/ThermalSubsystem/LeakDetection/LeakDetectors";

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
                mac: Some(MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac")),
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
                    scenario: "offline detectors report as sensor failures",
                    input: vec![
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/UnavailableOffline",
                            "Id": "UnavailableOffline",
                            "Name": "Unavailable offline leak detector",
                            "DetectorState": "Unavailable",
                            "Status": { "State": "UnavailableOffline" }
                        }"#,
                        r#"{
                            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/StandbyOffline",
                            "Id": "StandbyOffline",
                            "Name": "Standby offline leak detector",
                            "DetectorState": "Critical",
                            "Status": { "State": "StandbyOffline" },
                            "UserLabel": "Cold plate"
                        }"#,
                    ],
                    expect: expected_report(
                        vec![],
                        vec![
                            AlertSummary {
                                probe_id: Probe::LeakDetection,
                                target: Some(
                                    "/redfish/v1/Chassis/System/LeakDetectors/UnavailableOffline"
                                        .to_string(),
                                ),
                                message: "Leak detector '/redfish/v1/Chassis/System/LeakDetectors/UnavailableOffline' is offline: unavailable_offline".to_string(),
                                classifications: vec![Classification::SensorFailure],
                            },
                            AlertSummary {
                                probe_id: Probe::LeakDetection,
                                target: Some("Cold plate".to_string()),
                                message: "Leak detector 'Cold plate' is offline: standby_offline"
                                    .to_string(),
                                classifications: vec![Classification::SensorFailure],
                            },
                        ],
                    ),
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
    fn aggregate_detector_is_ignored_only_beside_individual_detectors() {
        let on = |chassis: &str, names: &[&str]| -> Vec<String> {
            names
                .iter()
                .map(|name| {
                    format!(
                        "/redfish/v1/Chassis/{chassis}/ThermalSubsystem/LeakDetection/LeakDetectors/{name}"
                    )
                })
                .collect()
        };
        // input: each chassis's detectors; expect: (reported, ignored aggregates)
        check_values(
            [
                Check {
                    scenario: "an aggregate beside individual detectors is ignored",
                    input: vec![on(
                        "Chassis_0",
                        &[
                            "Chassis_0_LeakDetector_0_ColdPlate",
                            "leakage_aggr",
                            "Chassis_0_LeakDetector_0_Manifold",
                        ],
                    )],
                    expect: (
                        on(
                            "Chassis_0",
                            &[
                                "Chassis_0_LeakDetector_0_ColdPlate",
                                "Chassis_0_LeakDetector_0_Manifold",
                            ],
                        ),
                        on("Chassis_0", &["leakage_aggr"]),
                    ),
                },
                Check {
                    scenario: "an aggregate that is the only detector is reported",
                    input: vec![on("Chassis_0", &["leakage_aggr"])],
                    expect: (on("Chassis_0", &["leakage_aggr"]), vec![]),
                },
                Check {
                    scenario: "individual detectors on another chassis do not hide an aggregate",
                    input: vec![
                        on("Chassis_0", &["leakage_aggr"]),
                        on(
                            "Chassis_1",
                            &["Chassis_1_LeakDetector_0_ColdPlate", "leakage_aggr"],
                        ),
                    ],
                    expect: (
                        [
                            on("Chassis_0", &["leakage_aggr"]),
                            on("Chassis_1", &["Chassis_1_LeakDetector_0_ColdPlate"]),
                        ]
                        .concat(),
                        on("Chassis_1", &["leakage_aggr"]),
                    ),
                },
                Check {
                    scenario: "ids that differ from a configured aggregate id are individual detectors",
                    input: vec![on(
                        "Chassis_0",
                        &["Leakage_Aggr", "leakage_aggr_1", "leakage_aggr"],
                    )],
                    expect: (
                        on("Chassis_0", &["Leakage_Aggr", "leakage_aggr_1"]),
                        on("Chassis_0", &["leakage_aggr"]),
                    ),
                },
            ],
            |chassis| {
                let aggregate_ids =
                    AggregateDetectorIds::from(BTreeSet::from(["leakage_aggr".to_string()]));
                let discovered: DiscoveredDetectors = chassis
                    .into_iter()
                    .map(|detectors| {
                        aggregate_ids.split(detectors.into_iter().map(ODataId::from).collect())
                    })
                    .collect();
                let paths =
                    |ids: Vec<ODataId>| ids.iter().map(ToString::to_string).collect::<Vec<_>>();
                (
                    paths(discovered.reported),
                    paths(discovered.ignored_aggregates),
                )
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
        let unreadable = [(
            ODataId::from(format!("{DETECTORS_PATH}/leakage2")),
            "invalid type: floating point `0.0`, expected i64".to_string(),
        )];

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
                    message: format!(
                        "Leak detector '{DETECTORS_PATH}/leakage2' could not be read: invalid type: floating point `0.0`, expected i64"
                    ),
                    classifications: vec![Classification::SensorFailure],
                }],
            ),
            "an unreadable detector must stay visible instead of silently shortening the batch"
        );
    }

    #[test]
    fn unreachable_bmc_keeps_previous_report_unless_a_leak_was_read() {
        let critical = r#"{
            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Critical",
            "Id": "Critical",
            "Name": "Critical leak detector",
            "DetectorState": "Critical",
            "Status": { "State": "Enabled" }
        }"#;
        let offline = r#"{
            "@odata.id": "/redfish/v1/Chassis/System/LeakDetectors/Offline",
            "Id": "Offline",
            "Name": "Offline leak detector",
            "DetectorState": "Unavailable",
            "Status": { "State": "UnavailableOffline" }
        }"#;
        // input: (unreadable detectors, of which connection failures, detector read)
        check_values(
            [
                Check {
                    scenario: "a decode failure alone is published",
                    input: (1, 0, None),
                    expect: true,
                },
                Check {
                    scenario: "connection failures alone keep the previous report",
                    input: (2, 2, None),
                    expect: false,
                },
                Check {
                    scenario: "a decode failure is published alongside a connection failure",
                    input: (2, 1, None),
                    expect: true,
                },
                Check {
                    scenario: "a leak read before the connection failed is published",
                    input: (1, 1, Some(critical)),
                    expect: true,
                },
                Check {
                    scenario: "an offline detector is published alongside a connection failure",
                    input: (1, 1, Some(offline)),
                    expect: true,
                },
            ],
            |(unreadable, connection_failures, read)| {
                DetectorReads {
                    detectors: Vec::from_iter(read.map(|json| {
                        Arc::new(serde_json::from_str(json).expect("valid leak detector"))
                    })),
                    unreadable: (0..unreadable)
                        .map(|index| {
                            (
                                ODataId::from(format!("{DETECTORS_PATH}/unreadable{index}")),
                                "unreadable".to_string(),
                            )
                        })
                        .collect(),
                    connection_failures,
                }
                .into_report(&context())
                .is_some()
            },
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

    /// A BMC that serves each body at its path and answers 404 for anything else.
    fn router_bmc(bodies: HashMap<String, String>) -> Arc<TestBmc> {
        let router = Router::new()
            .fallback(detector_response)
            .with_state(Arc::new(bodies));
        Arc::new(TestBmc::new(
            AxumRouterHttpClient::new(router),
            Url::parse("https://leak-detector-test.local").expect("test URL should parse"),
            BmcCredentials::new("root".to_string(), "password".to_string()),
            CacheSettings::with_capacity(8),
        ))
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
        let collector = LeakDetectorCollector::new_runner(
            router_bmc(bodies),
            Arc::new(test_endpoint(mac("42:9e:b1:bd:9d:dd"))),
            LeakDetectorCollectorConfig {
                data_sink: None,
                state_refresh_interval: Duration::from_secs(60),
                aggregate_detector_ids: AggregateDetectorIds::default(),
            },
        )
        .expect("leak detector collector should build");

        let detector_ids: Vec<ODataId> = detectors
            .iter()
            .map(|(id, _)| ODataId::from(format!("{DETECTORS_PATH}/{id}")))
            .collect();
        let DetectorReads {
            detectors: fetched,
            unreadable,
            connection_failures,
        } = collector.fetch_leak_detectors(&detector_ids).await;

        let fetched_ids: Vec<String> = fetched
            .iter()
            .map(|detector| detector.odata_id().to_string())
            .collect();
        let unreadable_ids: Vec<String> = unreadable.iter().map(|(id, _)| id.to_string()).collect();
        assert_eq!(
            (fetched_ids, unreadable_ids, connection_failures),
            (
                vec![
                    format!("{DETECTORS_PATH}/leakage1"),
                    format!("{DETECTORS_PATH}/leakage3"),
                ],
                vec![format!("{DETECTORS_PATH}/leakage2")],
                0,
            ),
            "a detector this client cannot decode should be named, not dropped from the batch, \
             and is not a connection failure"
        );
    }

    #[derive(Default)]
    struct ReportCapturingSink {
        reports: Mutex<Vec<HealthReport>>,
    }

    impl DataSink for ReportCapturingSink {
        fn sink_type(&self) -> &'static str {
            "report_capturing_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            if let CollectorEvent::HealthReport(report) = event {
                self.reports
                    .lock()
                    .expect("report lock should not be poisoned")
                    .push(report.as_ref().clone());
            }
            Ok(())
        }
    }

    /// The Redfish tree of the Wiwynn GB200 tray from #9199: both individual
    /// leak detectors went offline while the BMC's `leakage_aggr` kept
    /// reporting a critical leak.
    fn wiwynn_tray_with_offline_detectors() -> HashMap<String, String> {
        let thermal_subsystem = format!("{WIWYNN_CHASSIS_PATH}/ThermalSubsystem");
        let leak_detection = format!("{thermal_subsystem}/LeakDetection");
        let detector = |id: &str| format!("{WIWYNN_DETECTORS_PATH}/{id}");
        let offline_detector = |id: &str| {
            json!({
                "@odata.id": detector(id),
                "@odata.type": "#LeakDetector.v1_1_0.LeakDetector",
                "Id": id,
                "Name": id,
                "DetectorState": "Unavailable",
                "LeakDetectorType": "Moisture",
                "Status": { "State": "UnavailableOffline" }
            })
        };
        [
            (
                "/redfish/v1".to_string(),
                json!({
                    "@odata.id": "/redfish/v1",
                    "@odata.type": "#ServiceRoot.v1_15_0.ServiceRoot",
                    "Id": "RootService",
                    "Name": "Root Service",
                    "Chassis": { "@odata.id": "/redfish/v1/Chassis" },
                    "Links": {
                        "Sessions": { "@odata.id": "/redfish/v1/SessionService/Sessions" }
                    }
                }),
            ),
            (
                "/redfish/v1/Chassis".to_string(),
                json!({
                    "@odata.id": "/redfish/v1/Chassis",
                    "@odata.type": "#ChassisCollection.ChassisCollection",
                    "Name": "Chassis Collection",
                    "Members": [{ "@odata.id": WIWYNN_CHASSIS_PATH }]
                }),
            ),
            (
                WIWYNN_CHASSIS_PATH.to_string(),
                json!({
                    "@odata.id": WIWYNN_CHASSIS_PATH,
                    "@odata.type": "#Chassis.v1_23_0.Chassis",
                    "Id": "Chassis_0",
                    "Name": "Chassis_0",
                    "ChassisType": "RackMount",
                    "ThermalSubsystem": { "@odata.id": thermal_subsystem }
                }),
            ),
            (
                thermal_subsystem.clone(),
                json!({
                    "@odata.id": thermal_subsystem,
                    "@odata.type": "#ThermalSubsystem.v1_3_0.ThermalSubsystem",
                    "Id": "ThermalSubsystem",
                    "Name": "Thermal Subsystem",
                    "LeakDetection": { "@odata.id": leak_detection }
                }),
            ),
            (
                leak_detection.clone(),
                json!({
                    "@odata.id": leak_detection,
                    "@odata.type": "#LeakDetection.v1_1_0.LeakDetection",
                    "Id": "LeakDetection",
                    "Name": "Leak Detection",
                    "LeakDetectors": { "@odata.id": WIWYNN_DETECTORS_PATH }
                }),
            ),
            (
                WIWYNN_DETECTORS_PATH.to_string(),
                json!({
                    "@odata.id": WIWYNN_DETECTORS_PATH,
                    "@odata.type": "#LeakDetectorCollection.LeakDetectorCollection",
                    "Name": "Leak Detector Collection",
                    "Members": [
                        { "@odata.id": detector("Chassis_0_LeakDetector_0_ColdPlate") },
                        { "@odata.id": detector("leakage_aggr") },
                        { "@odata.id": detector("Chassis_0_LeakDetector_0_Manifold") }
                    ]
                }),
            ),
            (
                detector("Chassis_0_LeakDetector_0_ColdPlate"),
                offline_detector("Chassis_0_LeakDetector_0_ColdPlate"),
            ),
            (
                detector("Chassis_0_LeakDetector_0_Manifold"),
                offline_detector("Chassis_0_LeakDetector_0_Manifold"),
            ),
            (
                detector("leakage_aggr"),
                json!({
                    "@odata.id": detector("leakage_aggr"),
                    "@odata.type": "#LeakDetector.v1_1_0.LeakDetector",
                    "Id": "leakage_aggr",
                    "Name": "leakage_aggr",
                    "DetectorState": "Critical",
                    "Status": { "Health": "Critical", "State": "Enabled" }
                }),
            ),
        ]
        .into_iter()
        .map(|(path, body)| (path, body.to_string()))
        .collect()
    }

    #[tokio::test]
    async fn aggregate_beside_offline_detectors_raises_no_leak() {
        let detector = |id: &str| format!("{WIWYNN_DETECTORS_PATH}/{id}");
        let offline = |id: &str| AlertSummary {
            probe_id: Probe::LeakDetection,
            target: Some(detector(id)),
            message: format!(
                "Leak detector '{}' is offline: unavailable_offline",
                detector(id)
            ),
            classifications: vec![Classification::SensorFailure],
        };
        // input: configured aggregate detector ids; expect: alerts of the one
        // published report
        check_cases_async(
            [
                Case {
                    scenario: "the configured aggregate is ignored and the offline detectors are reported",
                    input: BTreeSet::from(["leakage_aggr".to_string()]),
                    expect: Yields(vec![
                        offline("Chassis_0_LeakDetector_0_ColdPlate"),
                        offline("Chassis_0_LeakDetector_0_Manifold"),
                    ]),
                },
                Case {
                    scenario: "with no aggregate ids configured the aggregate raises the false leak",
                    input: BTreeSet::new(),
                    expect: Yields(vec![
                        offline("Chassis_0_LeakDetector_0_ColdPlate"),
                        AlertSummary {
                            probe_id: Probe::LeakDetection,
                            target: Some(detector("leakage_aggr")),
                            message: format!(
                                "Leak detector '{}' reports critical",
                                detector("leakage_aggr")
                            ),
                            classifications: vec![Classification::LeakDetector],
                        },
                        offline("Chassis_0_LeakDetector_0_Manifold"),
                    ]),
                },
            ],
            |aggregate_detector_ids| async move {
                let sink = Arc::new(ReportCapturingSink::default());
                let mut collector = LeakDetectorCollector::new_runner(
                    router_bmc(wiwynn_tray_with_offline_detectors()),
                    Arc::new(test_endpoint(mac("42:9e:b1:bd:9d:dd"))),
                    LeakDetectorCollectorConfig {
                        data_sink: Some(sink.clone()),
                        state_refresh_interval: Duration::from_secs(60),
                        aggregate_detector_ids: aggregate_detector_ids.into(),
                    },
                )
                .map_err(|error| error.to_string())?;
                collector
                    .run_iteration()
                    .await
                    .map_err(|error| error.to_string())?;

                let reports = std::mem::take(
                    &mut *sink
                        .reports
                        .lock()
                        .expect("report lock should not be poisoned"),
                );
                let [report] = <[HealthReport; 1]>::try_from(reports)
                    .map_err(|reports| format!("expected one report, got {}", reports.len()))?;
                Ok::<_, String>(ReportSummary::from(report).alerts)
            },
        )
        .await;
    }
}

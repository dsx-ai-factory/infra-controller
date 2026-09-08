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

use carbide_uuid::nvlink::NvLinkDomainId;
use rpc::protos::nmx_c::NmxControllerHealth;

use super::{CollectorEvent, EventContext, EventProcessor};
use crate::sink::{
    HealthReport, HealthReportAlert, HealthReportSuccess, HealthReportTarget, LogRecord, Probe,
    ReportSource,
};

const DOMAIN_STATE_NOTIFICATION: &str = "domain_state_info";
const SUCCESS_RETURN_CODE: &str = "NMX_ST_SUCCESS";

/// Derives NVLink domain health reports from generated NMX-C log events.
#[derive(Default)]
pub struct NmxcDomainStateProcessor;

impl NmxcDomainStateProcessor {
    /// Creates a processor that derives NVLink domain reports from NMX-C state logs.
    pub fn new() -> Self {
        Self
    }

    fn attr<'a>(record: &'a LogRecord, key: &str) -> Option<&'a str> {
        record
            .attributes
            .iter()
            .find(|(name, _)| name.as_ref() == key)
            .map(|(_, value)| value.as_str())
    }

    fn report(context: &EventContext, record: &LogRecord) -> Option<HealthReport> {
        if context.collector_type != "nmxc"
            || Self::attr(record, "notification") != Some(DOMAIN_STATE_NOTIFICATION)
            || Self::attr(record, "return_code") != Some(SUCCESS_RETURN_CODE)
        {
            return None;
        }

        let expected_domain_id = context.nvlink_domain_uuid()?;

        let reported_domain_id = Self::attr(record, "domain_uuid")?
            .parse::<NvLinkDomainId>()
            .ok()?;

        if reported_domain_id != expected_domain_id {
            tracing::warn!(
                %expected_domain_id,
                %reported_domain_id,
                "NMX-C domain state does not match endpoint metadata"
            );

            return None;
        }

        let health =
            NmxControllerHealth::from_str_name(Self::attr(record, "nmx_controller_health")?)?;

        let (successes, alerts) = match health {
            NmxControllerHealth::Healthy => (
                vec![HealthReportSuccess {
                    attribution: None,
                    probe_id: Probe::NmxControllerHealth,
                    target: None,
                }],
                Vec::new(),
            ),
            NmxControllerHealth::Unhealthy | NmxControllerHealth::UnhealthyDbCorrupted => (
                Vec::new(),
                vec![HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::NmxControllerHealth,
                    target: None,
                    message: format!("NMX-C controller health is {}", health.as_str_name()),
                    classifications: Vec::new(),
                }],
            ),
            NmxControllerHealth::Unknown | NmxControllerHealth::Degraded => return None,
        };

        Some(HealthReport {
            source: ReportSource::NmxcDomainState,
            target: Some(HealthReportTarget::NvLinkDomain),
            observed_at: Some(chrono::Utc::now()),
            successes,
            alerts,
        })
    }
}

impl EventProcessor for NmxcDomainStateProcessor {
    fn processor_type(&self) -> &'static str {
        "nmxc_domain_state_processor"
    }

    fn process_event(&self, context: &EventContext, event: &CollectorEvent) -> Vec<CollectorEvent> {
        let CollectorEvent::Log(record) = event else {
            return Vec::new();
        };

        Self::report(context, record)
            .map(|report| vec![CollectorEvent::HealthReport(Arc::new(report))])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::value_scenarios;
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, SwitchData, SwitchEndpointRole};
    use crate::sink::LogSeverity;

    const DOMAIN_UUID: &str = "9f4b45ec-705a-4af4-89f7-a112bc9c8f4e";

    fn context() -> EventContext {
        EventContext {
            endpoint_key: "00:11:22:33:44:55".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: None,
                mac: MacAddress::from_str("00:11:22:33:44:55").expect("valid MAC address"),
            },
            collector_type: "nmxc",
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: None,
                serial: "SW-001".to_string(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: Some(DOMAIN_UUID.parse().expect("valid domain ID")),
                endpoint_role: SwitchEndpointRole::Host,
                is_primary: true,
                nmxc_enabled: true,
                nmxt_enabled: false,
            })),
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn log(health: NmxControllerHealth) -> CollectorEvent {
        CollectorEvent::Log(Box::new(LogRecord {
            body: "NMX-C domain state".to_string(),
            severity: LogSeverity::Info,
            attributes: vec![
                (
                    Cow::Borrowed("notification"),
                    DOMAIN_STATE_NOTIFICATION.to_string(),
                ),
                (
                    Cow::Borrowed("return_code"),
                    SUCCESS_RETURN_CODE.to_string(),
                ),
                (Cow::Borrowed("domain_uuid"), DOMAIN_UUID.to_string()),
                (
                    Cow::Borrowed("nmx_controller_health"),
                    health.as_str_name().to_string(),
                ),
            ],
            diagnostic_record: None,
        }))
    }

    #[derive(Debug, PartialEq)]
    enum ResultSummary {
        Alert,
        None,
        Success,
    }

    fn summary(health: NmxControllerHealth) -> ResultSummary {
        let events = NmxcDomainStateProcessor::new().process_event(&context(), &log(health));

        let Some(CollectorEvent::HealthReport(report)) = events.first() else {
            return ResultSummary::None;
        };

        if report.alerts.is_empty() {
            ResultSummary::Success
        } else {
            ResultSummary::Alert
        }
    }

    #[test]
    fn controller_health_maps_to_domain_reports() {
        value_scenarios!(
            run = summary;

            "healthy clears" {
                NmxControllerHealth::Healthy => ResultSummary::Success,
            }

            "unhealthy alerts" {
                NmxControllerHealth::Unhealthy => ResultSummary::Alert,
            }

            "database corruption alerts" {
                NmxControllerHealth::UnhealthyDbCorrupted => ResultSummary::Alert,
            }

            "degraded remains log only" {
                NmxControllerHealth::Degraded => ResultSummary::None,
            }

            "unknown remains log only" {
                NmxControllerHealth::Unknown => ResultSummary::None,
            }
        );
    }

    #[test]
    fn mismatched_domain_is_not_reported() {
        let mut context = context();

        let EndpointMetadata::Switch(switch) = context.metadata.as_mut().expect("switch metadata")
        else {
            panic!("expected switch metadata");
        };

        switch.nvlink_domain_uuid = Some(
            "00000000-0000-0000-0000-000000000001"
                .parse()
                .expect("valid domain ID"),
        );

        assert!(
            NmxcDomainStateProcessor::new()
                .process_event(&context, &log(NmxControllerHealth::Unhealthy))
                .is_empty()
        );
    }
}

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

use std::collections::HashMap;
use std::sync::Arc;

use ::rpc::measured_boot::FromGrpc;
use askama::Template;
use axum::extract::{Form, Path as AxumPath, State as AxumState};
use axum::response::{Html, IntoResponse};
use carbide_api_core::Api;
use carbide_uuid::measured_boot::MeasurementReportId;
use hyper::http::StatusCode;
use measured_boot::site::{MachineAttestationSummary, MachineAttestationSummaryList};
use measured_boot::{
    bundle as mbbundle, journal as mbjournal, profile as mbprofile, report as mbreport,
};
use rpc::forge::forge_server::Forge;
use rpc::protos::measured_boot as mbprotos;

use super::{Base, filters};

const PCR_SLOT_MAX_NUM: usize = 12;

const PCR_SLOT_DESCRIPTIONS: [&str; PCR_SLOT_MAX_NUM] = [
    "Core System Firmware executable code (aka Firmware)", // PCR0
    "Core System Firmware data (aka UEFI settings)",       // PCR1
    "Extended or pluggable executable code (aka OpROMs)",  // PCR2
    "Extended or pluggable firmware data",                 // PCR3
    "Boot Manager Code and Boot Attempts",                 // PCR4
    "Boot Manager Configuration and Data",                 // PCR5
    "Resume from S4 and S5 Power State Events",            // PCR6
    "Secure Boot State. Contains the full contents of PK/KEK/db, as well as the specific certificates used to validate each boot application", // PCR7
    "The kernel being booted by the iPXE bootloader", // PCR8
    "initrd/initramfs region",                        // PCR9,
    "Additional configuration and data",              // PCR10
    "Additional configuration and data",              // PCR11
];

const ATTESTED: &str = "Attested";
const NOT_ATTESTED: &str = "Attestation Failed";

#[derive(Template)]
#[template(path = "attestation_summary.html")]
struct AttestationSummary {
    attestations: Vec<MachineAttestationSummary>,
}

#[derive(Template)]
#[template(path = "attestation_results.html")]
struct AttestationResults {
    journal_time: String,
    journal_id: String,
    attestation_status: String,
    attestation_table: Vec<AttestationPcr>,
    report_ts: String,
    bundle_ts: String,
    bundle_status: String,
    bundle_id: String,
    report_id: String,
    profile_name: String,
    profile_id: String,
    machine_id: String,
    profile_attributes: Vec<(String, String)>,
}

#[derive(Default, Clone)]
struct AttestationPcr {
    pcr_slot: i16,
    report_value: String,
    bundle_value: String,
    description: String,
}

/// View attestation results
pub(super) async fn show_attestation_results(
    AxumState(state): AxumState<Arc<Api>>,
    AxumPath(machine_id): AxumPath<String>,
) -> impl IntoResponse {
    // 1. get latest journal entry, report, bundle
    // 2. populate AttestationResults and return it

    // 1.
    // get latest journal

    let latest_journal = match get_latest_journal_for_machine_id(&state, &machine_id).await {
        Ok(journal) => journal,
        Err(err_response) => return err_response,
    };

    // get report
    let request = tonic::Request::new(mbprotos::ShowMeasurementReportForIdRequest {
        report_id: Some(latest_journal.report_id),
    });
    let report = match state.show_measurement_report_for_id(request).await {
        Ok(resp) => match resp.into_inner().report {
            Some(report) => match mbreport::MeasurementReport::from_grpc(report) {
                Ok(report) => report,
                Err(err) => {
                    tracing::error!(error = %err, "show_attestation_results");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("Error deserializing the report".to_string()),
                    );
                }
            },
            None => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("No report returned by the API".to_string()),
                );
            }
        },
        Err(err) => {
            tracing::error!(error = %err, "show_attestation_results");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Error getting measurement report".to_string()),
            );
        }
    };

    // get the bundle if it had been set
    let bundle = if let Some(bundle_id) = latest_journal.bundle_id {
        let request = tonic::Request::new(mbprotos::ShowMeasurementBundleRequest {
            selector: Some(
                mbprotos::show_measurement_bundle_request::Selector::BundleId(bundle_id),
            ),
        });
        let bundle = match state.show_measurement_bundle(request).await {
            Ok(resp) => match resp.into_inner().bundle {
                Some(bundle) => match mbbundle::MeasurementBundle::from_grpc(bundle) {
                    Ok(bundle) => bundle,
                    Err(err) => {
                        tracing::error!(error = %err, "show_attestation_results");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Html("Error deserializing the bundle".to_string()),
                        );
                    }
                },
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("MeasurementBundleId present, but no bundle found".to_string()),
                    );
                }
            },
            Err(err) => {
                tracing::error!(error = %err, "show_attestation_results");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Error getting measurement bundle".to_string()),
                );
            }
        };
        Some(bundle)
    } else {
        // try fetching the closest matching bundle if the full bundle had not been set
        let request = tonic::Request::new(mbprotos::FindClosestBundleMatchRequest {
            report_id: Some(report.report_id),
        });
        match state.find_closest_bundle_match(request).await {
            Ok(resp) => match resp.into_inner().bundle {
                Some(bundle) => match mbbundle::MeasurementBundle::from_grpc(bundle) {
                    Ok(bundle) => Some(bundle),
                    Err(err) => {
                        tracing::error!(error = %err, "show_attestation_results");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Html("Error deserializing the bundle".to_string()),
                        );
                    }
                },
                None => None,
            },
            Err(err) => {
                tracing::error!(error = %err, "show_attestation_results");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Error getting partially matching measurement bundle".to_string()),
                );
            }
        }
    };

    // get profile
    let profile = if let Some(profile_id) = latest_journal.profile_id {
        let request = tonic::Request::new(mbprotos::ShowMeasurementSystemProfileRequest {
            selector: Some(
                mbprotos::show_measurement_system_profile_request::Selector::ProfileId(profile_id),
            ),
        });

        match state.show_measurement_system_profile(request).await {
            Ok(resp) => match resp.into_inner().system_profile {
                Some(profile_pb) => {
                    match mbprofile::MeasurementSystemProfile::from_grpc(profile_pb) {
                        Ok(profile) => profile,
                        Err(err) => {
                            tracing::error!(error = %err, "show_attestation_results");
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Html("Error deserializing measurement system profile".to_string()),
                            );
                        }
                    }
                }
                None => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("No system measurement profile returned".to_string()),
                    );
                }
            },
            Err(err) => {
                tracing::error!(error = %err, "show_attestation_results");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("Error getting measurement system profile".to_string()),
                );
            }
        }
    } else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html("Journal is not linked to a profile".to_string()),
        );
    };

    // 2.
    let mut report_table = vec![AttestationPcr::default(); report.pcr_values().len()];
    // create a vector of AttestationPcrs and add it to AttestationResults
    for val in report.pcr_values() {
        let idx = val.pcr_register as usize;
        report_table[idx].pcr_slot = idx as i16;
        report_table[idx].report_value = val.sha_any.clone();
        report_table[idx].bundle_value = get_bundle_value_for_pcr_slot(&bundle, idx);
        report_table[idx].description = if idx < PCR_SLOT_DESCRIPTIONS.len() {
            PCR_SLOT_DESCRIPTIONS[idx].to_string()
        } else {
            "-".to_string()
        }
    }

    let (bundle_ts, bundle_status) = match &bundle {
        Some(bundle) => (bundle.ts.to_string(), bundle.state.to_string()),
        None => ("-".to_string(), "-".to_string()),
    };

    let attestation_results = AttestationResults {
        journal_time: latest_journal.ts.to_string(),
        journal_id: latest_journal.journal_id.to_string(),
        attestation_status: if latest_journal.bundle_id.is_some() {
            ATTESTED.to_string()
        } else {
            NOT_ATTESTED.to_string()
        },
        attestation_table: report_table,
        report_ts: report.ts.to_string(),
        bundle_ts,
        bundle_status,
        report_id: report.report_id.to_string(),
        bundle_id: match bundle {
            Some(bundle) => bundle.bundle_id.to_string(),
            None => "-".to_string(),
        },
        profile_name: profile.name,
        profile_id: profile.profile_id.to_string(),
        machine_id,
        profile_attributes: profile
            .attrs
            .iter()
            .map(|elem| (elem.key.clone(), elem.value.clone()))
            .collect(),
    };
    (StatusCode::OK, Html(attestation_results.render().unwrap()))
}

fn get_bundle_value_for_pcr_slot(
    bundle: &Option<mbbundle::MeasurementBundle>,
    pcr_slot: usize,
) -> String {
    match bundle {
        Some(bundle) => {
            for pcr_value in bundle.pcr_values() {
                if pcr_value.pcr_register as usize == pcr_slot {
                    return pcr_value.sha_any;
                }
            }
            "-".to_string()
        }
        None => "-".to_string(),
    }
}

async fn get_latest_journal_for_machine_id(
    state: &Arc<Api>,
    machine_id: &str,
) -> Result<mbjournal::MeasurementJournal, (StatusCode, Html<String>)> {
    let request = tonic::Request::new(mbprotos::ShowMeasurementJournalRequest {
        selector: Some(
            mbprotos::show_measurement_journal_request::Selector::LatestForMachineId(
                machine_id.to_string(),
            ),
        ),
    });

    let latest_journal = match state.show_measurement_journal(request).await {
        Ok(response) => match response.into_inner().journal {
            Some(journal_proto) => match mbjournal::MeasurementJournal::from_grpc(journal_proto) {
                Ok(journal) => journal,
                Err(err) => {
                    tracing::error!(error = %err, "get_latest_journal_for_machine_id");
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("Failed parsing MeasurementBundle protobuf".to_string()),
                    ));
                }
            },
            None => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Html("No latest journal found for machine id".to_string()),
                ));
            }
        },
        Err(err) => {
            tracing::error!(error = %err, "get_latest_journal_for_machine_id");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Error getting journal".to_string()),
            ));
        }
    };

    Ok(latest_journal)
}

pub(super) async fn show_attestation_summary(
    AxumState(state): AxumState<Arc<Api>>,
) -> impl IntoResponse {
    let request = tonic::Request::new(mbprotos::ListAttestationSummaryRequest {});

    let attestation_summary = match state.list_attestation_summary(request).await {
        Ok(response) => AttestationSummary {
            attestations: match MachineAttestationSummaryList::try_from(response.into_inner()) {
                Ok(attestations_list) => attestations_list.0,
                Err(err) => {
                    tracing::error!(error = %err, "show_attestation_summary");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("Error obtaining MachineAttestationSummaryList".to_string()),
                    );
                }
            },
        },
        Err(err) => {
            tracing::error!(error = %err, "show_attestation_summary");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Error calling list_attestation_summary".to_string()),
            );
        }
    };

    (StatusCode::OK, Html(attestation_summary.render().unwrap()))
}

pub(super) async fn submit_report_promotion(
    AxumState(state): AxumState<Arc<Api>>,
    Form(params): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut pcr_registers_to_promote: String = String::new();
    let mut first_element: bool = true;

    for key in params.keys() {
        match extract_pcr_slot_idx(key) {
            Ok(idx_opt) => {
                if let Some(idx) = idx_opt {
                    if first_element {
                        pcr_registers_to_promote += &idx;
                        first_element = false;
                    } else {
                        pcr_registers_to_promote += &format!(",{idx}");
                    }
                }
            }
            Err(err) => {
                return err;
            }
        }
    }

    let Some(Ok(report_id)) = params
        .get("reportname")
        .map(|r| r.parse::<MeasurementReportId>())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Html("report id not found".to_string()),
        );
    };

    let request = mbprotos::PromoteMeasurementReportRequest {
        report_id: Some(report_id),
        pcr_registers: pcr_registers_to_promote,
    };

    match state
        .promote_measurement_report(tonic::Request::new(request))
        .await
    {
        Ok(resp) => match resp.into_inner().bundle {
            Some(bundle) => (
                StatusCode::OK,
                Html(if let Some(bundle_id) = bundle.bundle_id {
                    format!("Bundle created with bundle id {bundle_id}")
                } else {
                    format!("Bundle created with name {}", bundle.name)
                }),
            ),
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Could not promote report to a bundle".to_string()),
            ),
        },
        Err(err) => {
            tracing::error!(error = %err, "submit_report_promotion");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("Error promoting report".to_string()),
            )
        }
    }
}

fn extract_pcr_slot_idx(potential_idx: &str) -> Result<Option<String>, (StatusCode, Html<String>)> {
    // if the first three letters are not PCR, just ignore
    if potential_idx.len() > 3 && potential_idx.starts_with("PCR") {
        // try and extract the idx
        let pcr_idx = &potential_idx[3..];
        match pcr_idx.to_string().parse::<usize>() {
            Ok(idx) => {
                if idx < PCR_SLOT_MAX_NUM {
                    return Ok(Some(pcr_idx.to_string()));
                } else {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("PCR index is out of range".to_string()),
                    ));
                }
            }
            Err(err) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html(format!("Error parsing PCR index: {err}")),
                ));
            }
        }
    }

    Ok(None)
}

impl super::Base for AttestationSummary {}
impl super::Base for AttestationResults {}
